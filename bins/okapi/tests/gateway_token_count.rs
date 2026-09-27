//! Responses 计数原生协议、无资金副作用、隔离权限与真实 Redis 准入。
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use okapi::gateway;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
struct Mock {
    calls: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
    reply: Arc<Mutex<(u16, String)>>,
    delay: Arc<AtomicU64>,
    source: Arc<Mutex<&'static str>>,
}

impl Mock {
    fn set(&self, status: u16, value: &Value) {
        *self.reply.lock().unwrap() = (status, value.to_string());
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

async fn mock_count(
    State(mock): State<Mock>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mock.calls.lock().unwrap().push((
        uri.path().to_owned(),
        headers,
        serde_json::from_slice(&body).unwrap(),
    ));
    tokio::time::sleep(Duration::from_millis(mock.delay.load(Ordering::SeqCst))).await;
    if uri.path().starts_with("/bad/") {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    assert!(
        uri.path().ends_with("/responses/input_tokens"),
        "must never generate: {uri}"
    );
    let (status, reply) = mock.reply.lock().unwrap().clone();
    let source = *mock.source.lock().unwrap();
    (
        StatusCode::from_u16(status).unwrap(),
        [
            ("content-type", "application/json"),
            ("location", "/bad/v1/responses/input_tokens"),
            ("x-okapi-token-count-source", source),
        ],
        reply,
    )
        .into_response()
}

struct Env {
    pg: sqlx::PgPool,
    state: gateway::state::AppState,
    gateway: SocketAddr,
    upstream: SocketAddr,
    token: String,
    key_id: i64,
    user_id: i64,
    channel_id: i64,
    channel_key: i64,
    model: String,
    mock: Mock,
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

async fn setup(provider: &str) -> Env {
    dotenvy::dotenv().ok();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(&pg, &format!("count-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-count-{suffix}");
    let key_id = okapi_store::provision::create_api_key(
        &pg,
        user_id,
        &hex::encode(Sha256::digest(token.as_bytes())),
        "sk-count",
    )
    .await
    .unwrap();
    let model = format!("count-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let mock = Mock {
        calls: Arc::default(),
        reply: Arc::new(Mutex::new((
            200,
            json!({"object":"response.input_tokens", "input_tokens":42}).to_string(),
        ))),
        delay: Arc::default(),
        source: Arc::new(Mutex::new("upstream")),
    };
    let upstream = serve(Router::new().fallback(mock_count).with_state(mock.clone())).await;
    let credential = if provider == "codex" {
        okapi_store::credential::OAuthCredential {
            access_token: "upstream-secret".to_owned(),
            refresh_token: "unused-refresh".to_owned(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            account_id: Some("count-account".to_owned()),
        }
        .to_plaintext()
    } else {
        "upstream-secret".to_owned()
    };
    let (channel_id, channel_key) = okapi_store::provision::create_channel(
        &pg,
        &format!("count-{suffix}"),
        provider,
        &format!("http://{upstream}/good/v1"),
        &credential,
        &[&model],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channels SET model_mapping=$2, settings=$3 WHERE id=$1")
        .bind(channel_id)
        .bind(json!({model.as_str():"gpt-4o"}))
        .bind(json!({"responses_native":true, "extra_headers":{"x-count-fixture":"kept"}}))
        .execute(&pg)
        .await
        .unwrap();
    let state = gateway::build_state(&database, &redis, "token-count-test", None, None)
        .await
        .unwrap();
    let candidates = okapi_store::channels::candidates_for_model(&pg, &model, &["default"], None)
        .await
        .unwrap();
    let binding =
        gateway::sched_redis::response_affinity::ResponseBinding::from_candidate(&candidates[0]);
    state
        .sched
        .response_binding_set(user_id, key_id, "resp-count", &binding)
        .await
        .unwrap();
    let gateway = serve(gateway::router(state.clone())).await;
    Env {
        pg,
        state,
        gateway,
        upstream,
        token,
        key_id,
        user_id,
        channel_id,
        channel_key,
        model,
        mock,
    }
}

impl Env {
    fn request(&self, value: &Value) -> reqwest::RequestBuilder {
        reqwest::Client::new()
            .post(format!("http://{}/v1/responses/input_tokens", self.gateway))
            .bearer_auth(&self.token)
            .json(value)
    }
    fn body(&self) -> Value {
        json!({"model":self.model, "input":"你好，count these tokens"})
    }
    async fn post(&self, value: &Value) -> reqwest::Response {
        self.request(value).send().await.unwrap()
    }
    async fn flush_auth(&self) {
        self.state
            .sched
            .auth_del(&hex::encode(Sha256::digest(self.token.as_bytes())))
            .await;
    }
    async fn assert_no_billing(&self) {
        assert_eq!(
            self.state
                .ledger
                .balance(self.user_id)
                .await
                .unwrap()
                .as_micros(),
            0
        );
        assert!(
            self.state
                .ledger
                .list_reservations(self.user_id)
                .await
                .unwrap()
                .is_empty()
        );
        for statement in [
            "SELECT count(*) FROM billing_records WHERE user_id=$1",
            "SELECT count(*) FROM billing_events WHERE user_id=$1",
        ] {
            let total: i64 = sqlx::query_scalar(statement)
                .bind(self.user_id)
                .fetch_one(&self.pg)
                .await
                .unwrap();
            assert_eq!(
                total, 0,
                "{statement}: counting must not create financial entries"
            );
        }
        let outbox: i64 =
            sqlx::query_scalar("SELECT count(*) FROM billing_outbox WHERE payload->>'user_id'=$1")
                .bind(self.user_id.to_string())
                .fetch_one(&self.pg)
                .await
                .unwrap();
        assert_eq!(outbox, 0);
        let snapshots: (i64, i64) = sqlx::query_as("SELECT k.used_micro, u.balance_micro FROM api_keys k JOIN users u ON u.id=k.user_id WHERE k.id=$1")
            .bind(self.key_id).fetch_one(&self.pg).await.unwrap();
        assert_eq!(
            snapshots,
            (0, 0),
            "key usage and persisted balance must not change"
        );
    }
}

#[tokio::test]
async fn native_count_preserves_context_maps_models_and_never_bills() {
    for provider in ["openai", "openai_compat", "codex"] {
        let env = setup(provider).await;
        let mut body = json!({
            "model":env.model, "stream":false, "instructions":"Keep context intact",
            "input":[{"role":"user","content":[{"type":"input_text","text":"hello"},{"type":"input_image","image_url":"https://image.example/a.png"},{"type":"input_file","file_id":"file-count"}]},
                {"type":"reasoning","encrypted_content":"opaque-context"}],
            "previous_response_id":"resp-count", "tools":[{"type":"function","name":"weather","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}],
            "reasoning":{"effort":"high"}, "text":{"format":{"type":"json_object"}}, "provider":{"allow_fallbacks":false}
        });
        let response = env
            .request(&body)
            .header("originator", "client-count")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-okapi-token-count-source"], "upstream");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(response.headers().contains_key("x-okapi-request-id"));
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"object":"response.input_tokens","input_tokens":42})
        );
        body["model"] = json!("gpt-4o");
        body.as_object_mut().unwrap().remove("stream");
        body.as_object_mut().unwrap().remove("provider");
        {
            let calls = env.mock.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, "/good/v1/responses/input_tokens");
            assert_eq!(calls[0].2, body);
            assert_eq!(calls[0].1["authorization"], "Bearer upstream-secret");
            assert_eq!(calls[0].1["x-count-fixture"], "kept");
            if provider == "codex" {
                assert_eq!(calls[0].1["chatgpt-account-id"], "count-account");
                assert_eq!(calls[0].1["originator"], "client-count");
            }
        }
        env.mock.set(
            200,
            &json!({"object":"response.input_tokens", "input_tokens":0}),
        );
        assert_eq!(
            env.post(&env.body()).await.json::<Value>().await.unwrap()["input_tokens"],
            0
        );
        env.assert_no_billing().await;
    }
}

#[tokio::test]
async fn count_validates_input_auth_model_pool_and_retention_before_upstream() {
    let env = setup("openai").await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/responses/input_tokens", env.gateway))
        .bearer_auth("wrong")
        .json(&env.body())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    for body in [
        json!([]),
        json!({}),
        json!({"model":null}),
        json!({"model":" "}),
        json!({"model":env.model,"input":5}),
        json!({"model":env.model,"input":[4]}),
        json!({"model":env.model,"stream":true}),
        json!({"model":env.model,"tools":{}}),
        json!({"model":env.model,"conversation":"conv","previous_response_id":"resp"}),
        json!({"model":env.model,"conversation":{"id":2}}),
    ] {
        assert_eq!(env.post(&body).await.status(), 400, "{body}");
    }
    assert_eq!(
        env.request(&env.body())
            .header("x-okapi-token-count-mode", "guess")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        env.post(&json!({"model":"count-nonexistent"}))
            .await
            .status(),
        404
    );
    sqlx::query("UPDATE api_keys SET model_allowlist='[\"different\"]' WHERE id=$1")
        .bind(env.key_id)
        .execute(&env.pg)
        .await
        .unwrap();
    env.flush_auth().await;
    assert_eq!(env.post(&env.body()).await.status(), 403);
    sqlx::query(
        "UPDATE api_keys SET model_allowlist=NULL, ip_allowlist='[\"192.0.2.1\"]'::jsonb WHERE id=$1",
    )
    .bind(env.key_id)
    .execute(&env.pg)
    .await
    .unwrap();
    env.flush_auth().await;
    assert_eq!(env.post(&env.body()).await.status(), 403);
    sqlx::query("UPDATE api_keys SET ip_allowlist=NULL WHERE id=$1")
        .bind(env.key_id)
        .execute(&env.pg)
        .await
        .unwrap();
    env.flush_auth().await;
    let mut body = env.body();
    body["provider"] = json!({"zdr":true});
    assert_eq!(env.post(&body).await.status(), 503);
    sqlx::query("DELETE FROM pool_channels WHERE channel_id=$1")
        .bind(env.channel_id)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(env.post(&env.body()).await.status(), 503);
    assert_eq!(
        env.request(&env.body())
            .header("x-okapi-token-count-mode", "estimate")
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(env.mock.count(), 0);
    env.assert_no_billing().await;
}

#[tokio::test]
async fn malformed_and_unsupported_upstreams_never_return_a_fake_count_or_generate() {
    let env = setup("openai").await;
    for output in [
        json!({}),
        json!({"object":"response.input_tokens"}),
        json!({"object":"response", "input_tokens":2}),
        json!({"object":"response.input_tokens", "input_tokens":-1}),
        json!({"object":"response.input_tokens", "input_tokens":1.5}),
        json!({"object":"response.input_tokens", "input_tokens":"42"}),
        json!({"object":"response.input_tokens", "input_tokens":4_294_967_296_u64}),
        json!({"object":"response.input_tokens", "input_tokens":42, "estimated":"true"}),
    ] {
        env.mock.set(200, &output);
        let response = env
            .request(&env.body())
            .header("x-okapi-token-count-mode", "auto")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 502, "{output}");
    }
    for output in ["not-json".to_owned(), "x".repeat(33 * 1024)] {
        *env.mock.reply.lock().unwrap() = (200, output);
        assert_eq!(env.post(&env.body()).await.status(), 502);
    }
    for (upstream, expected) in [
        (302, 502),
        (400, 400),
        (401, 502),
        (429, 429),
        (500, 502),
        (404, 501),
        (405, 501),
        (501, 501),
    ] {
        env.mock.set(
            upstream,
            &json!({"error":{"message":"upstream-private-secret"}}),
        );
        let before = env.mock.count();
        let response = env.post(&env.body()).await;
        assert_eq!(response.status(), expected);
        assert_eq!(
            env.mock.count(),
            before + 1,
            "must not follow redirects or generate fallback content"
        );
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("upstream-private-secret")
        );
    }
    assert!(
        env.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(path, _, _)| path.ends_with("/responses/input_tokens"))
    );
    env.assert_no_billing().await;
}

#[tokio::test]
async fn estimates_are_explicit_and_reject_unavailable_context() {
    let env = setup("openai").await;
    let body = env.body();
    let base = env
        .request(&body)
        .header("x-okapi-token-count-mode", "estimate")
        .send()
        .await
        .unwrap();
    assert_eq!(base.status(), 200);
    assert_eq!(
        base.headers()["x-okapi-token-count-source"],
        "local_estimate"
    );
    let base: Value = base.json().await.unwrap();
    assert_eq!(base["estimated"], true);
    assert!(base["input_tokens"].as_u64().unwrap() > 0);
    let mut rich = body.clone();
    rich["instructions"] = json!("Use these tools to answer the user's question accurately.");
    rich["tools"] = json!([{"type":"function","name":"weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}]);
    let rich: Value = env
        .request(&rich)
        .header("x-okapi-token-count-mode", "estimate")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rich["input_tokens"].as_u64() > base["input_tokens"].as_u64());
    for extra in [
        json!({"previous_response_id":"resp-private"}),
        json!({"conversation":"conv"}),
        json!({"prompt":{"id":"stored-prompt"}}),
        json!({"input":[{"type":"reasoning","encrypted_content":"secret"}]}),
        json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":"https://image.example/i.png"}]}]}),
        json!({"input":[{"role":"user","content":[{"type":"input_file","file_id":"file-1"}]}]}),
    ] {
        let mut request = body.clone();
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            env.request(&request)
                .header("x-okapi-token-count-mode", "estimate")
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(env.mock.count(), 0);
    env.mock.set(404, &json!({"error":"unsupported"}));
    let response = env
        .request(&body)
        .header("x-okapi-token-count-mode", "auto")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["estimated"], true);
    env.mock.set(500, &json!({"error":"temporarily broken"}));
    assert_eq!(
        env.request(&body)
            .header("x-okapi-token-count-mode", "auto")
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    env.assert_no_billing().await;
}

#[tokio::test]
async fn native_capability_selection_and_failover_stay_on_count_endpoint() {
    let env = setup("openai").await;
    let (bad, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("bad-{}", Uuid::new_v4()),
        "openai",
        &format!("http://{}/bad/v1", env.upstream),
        "upstream-secret",
        &[&env.model],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channels SET priority=100 WHERE id=$1")
        .bind(bad)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(env.post(&env.body()).await.status(), 200);
    assert_eq!(
        env.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|x| x.0.clone())
            .collect::<Vec<_>>(),
        [
            "/bad/v1/responses/input_tokens",
            "/good/v1/responses/input_tokens"
        ]
    );
    let mut body = env.body();
    body["provider"] = json!({"allow_fallbacks":false});
    assert_eq!(env.post(&body).await.status(), 502);
    assert_eq!(env.mock.count(), 3);
    sqlx::query("UPDATE channels SET capabilities='{\"input_tokens\":false}' WHERE id=$1")
        .bind(bad)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(env.post(&env.body()).await.status(), 200);
    assert_eq!(env.mock.count(), 4);
    sqlx::query("UPDATE channels SET settings='{\"responses_native\":false}' WHERE id=$1")
        .bind(env.channel_id)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(env.post(&env.body()).await.status(), 501);
    assert_eq!(env.mock.count(), 4);
    assert_eq!(
        env.request(&env.body())
            .header("x-okapi-token-count-mode", "auto")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    env.assert_no_billing().await;
}

#[tokio::test]
async fn count_rate_limits_are_real_without_balance_reservations() {
    for (statement, expected) in [
        (
            "UPDATE api_keys SET rpm_limit=1 WHERE id=$1",
            "token_count_rpm",
        ),
        (
            "UPDATE api_keys SET rpd_limit=1 WHERE id=$1",
            "token_count_rpd",
        ),
    ] {
        let env = setup("openai").await;
        sqlx::query(statement)
            .bind(env.key_id)
            .execute(&env.pg)
            .await
            .unwrap();
        assert_eq!(env.post(&env.body()).await.status(), 200);
        let response = env.post(&env.body()).await;
        assert_eq!(response.status(), 429);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["param"],
            expected
        );
        assert_eq!(env.mock.count(), 1);
        env.assert_no_billing().await;
    }
}

async fn wait_calls(env: &Env, count: usize) {
    for _ in 0..100 {
        if env.mock.count() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("upstream call not reached");
}

#[tokio::test]
async fn count_concurrency_releases_on_success_failure_and_timeout() {
    let env = setup("openai").await;
    sqlx::query("UPDATE api_keys SET max_concurrency=1 WHERE id=$1")
        .bind(env.key_id)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channel_keys SET max_concurrency=1 WHERE id=$1")
        .bind(env.channel_key)
        .execute(&env.pg)
        .await
        .unwrap();
    env.mock.delay.store(400, Ordering::SeqCst);
    let request = env.request(&env.body());
    let first = tokio::spawn(async move { request.send().await.unwrap() });
    wait_calls(&env, 1).await;
    let blocked = env.post(&env.body()).await;
    assert_eq!(blocked.status(), 429);
    assert_eq!(
        blocked.json::<Value>().await.unwrap()["error"]["param"],
        "token_count_concurrency"
    );
    assert_eq!(first.await.unwrap().status(), 200);
    env.mock.delay.store(0, Ordering::SeqCst);
    env.mock.set(503, &json!({"error":"temporary"}));
    assert_eq!(env.post(&env.body()).await.status(), 502);
    env.mock.set(
        200,
        &json!({"object":"response.input_tokens","input_tokens":9}),
    );
    assert_eq!(env.post(&env.body()).await.status(), 200);
    assert!(env.state.sched.acquire_slot(env.channel_key, Some(1)).await);
    let channel_blocked = env.post(&env.body()).await;
    assert_eq!(channel_blocked.status(), 429);
    assert_eq!(
        channel_blocked.json::<Value>().await.unwrap()["error"]["param"],
        "channel_concurrency"
    );
    env.state.sched.release_slot(env.channel_key, Some(1)).await;
    sqlx::query("UPDATE channels SET retry_policy='{\"first_output_timeout_secs\":5}' WHERE id=$1")
        .bind(env.channel_id)
        .execute(&env.pg)
        .await
        .unwrap();
    env.mock.delay.store(6000, Ordering::SeqCst);
    assert_eq!(env.post(&env.body()).await.status(), 504);
    env.mock.delay.store(0, Ordering::SeqCst);
    assert_eq!(env.post(&env.body()).await.status(), 200);
    env.assert_no_billing().await;
}

#[tokio::test]
async fn counting_obeys_group_model_and_channel_rate_limits() {
    let env = setup("openai").await;
    env.state
        .settings_cache
        .insert(
            "model_rpm_limits".to_owned(),
            Arc::new(Some(json!({env.model.as_str():1}))),
        )
        .await;
    assert_eq!(env.post(&env.body()).await.status(), 200);
    let limited = env.post(&env.body()).await;
    assert_eq!(limited.status(), 429);
    assert_eq!(
        limited.json::<Value>().await.unwrap()["error"]["param"],
        "model_rpm"
    );
    assert_eq!(env.mock.count(), 1);
    env.assert_no_billing().await;

    let env = setup("openai").await;
    let group = format!("cg-{}", &Uuid::new_v4().simple().to_string()[..12]);
    okapi_store::admin::upsert_price_group(
        &env.pg,
        okapi_store::admin::PriceGroupInput {
            group_code: &group,
            group_ratio: "1",
            description: "",
            pool_code: None,
            self_select: false,
            rpm_limit: Some(1),
            rph_limit: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE api_keys SET group_override=$2 WHERE id=$1")
        .bind(env.key_id)
        .bind(group)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(env.post(&env.body()).await.status(), 200);
    let limited = env.post(&env.body()).await;
    assert_eq!(limited.status(), 429);
    assert_eq!(
        limited.json::<Value>().await.unwrap()["error"]["param"],
        "group_rpm"
    );
    assert_eq!(env.mock.count(), 1);
    env.assert_no_billing().await;

    let env = setup("openai").await;
    sqlx::query("UPDATE channel_keys SET rpm_limit=1 WHERE id=$1")
        .bind(env.channel_key)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(env.post(&env.body()).await.status(), 200);
    let limited = env.post(&env.body()).await;
    assert_eq!(limited.status(), 429);
    assert_eq!(
        limited.json::<Value>().await.unwrap()["error"]["param"],
        "channel_rpm"
    );
    assert_eq!(env.mock.count(), 1);
    env.assert_no_billing().await;
}

#[tokio::test]
async fn cancelling_count_handler_releases_both_leases() {
    let env = setup("openai").await;
    sqlx::query("UPDATE api_keys SET max_concurrency=1 WHERE id=$1")
        .bind(env.key_id)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channel_keys SET max_concurrency=1 WHERE id=$1")
        .bind(env.channel_key)
        .execute(&env.pg)
        .await
        .unwrap();
    env.mock.delay.store(5000, Ordering::SeqCst);
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", env.token).parse().unwrap(),
    );
    let task = tokio::spawn(gateway::token_count::responses_input_tokens(
        State(env.state.clone()),
        headers,
        Bytes::from(env.body().to_string()),
    ));
    wait_calls(&env, 1).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    env.mock.delay.store(0, Ordering::SeqCst);
    let mut recovered = false;
    for _ in 0..20 {
        let response = env.post(&env.body()).await;
        if response.status() == 200 {
            recovered = true;
            break;
        }
        assert_eq!(response.status(), 429);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        recovered,
        "cancelled handler leaked request/channel concurrency"
    );
    assert_eq!(env.mock.count(), 2);
    assert!(env.state.sched.acquire_slot(env.channel_key, Some(1)).await);
    assert!(
        !env.state.sched.acquire_slot(env.channel_key, Some(1)).await,
        "double release must not create a second free slot"
    );
    env.state.sched.release_slot(env.channel_key, Some(1)).await;
    env.assert_no_billing().await;
}

#[tokio::test]
async fn aliases_use_canonical_permissions_and_upstream_estimation_stays_visible() {
    let env = setup("openai").await;
    let alias = format!("count-alias-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO model_aliases(pattern, target_model, priority) VALUES($1,$2,0)")
        .bind(&alias)
        .bind(&env.model)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE api_keys SET model_allowlist=$2 WHERE id=$1")
        .bind(env.key_id)
        .bind(json!([env.model]))
        .execute(&env.pg)
        .await
        .unwrap();
    env.mock.set(
        200,
        &json!({"object":"response.input_tokens","input_tokens":50,"estimated":true}),
    );
    let response = env
        .post(&json!({"model":alias,"conversation":{"id":"conv-1"},"input":"next"}))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-okapi-token-count-source"], "upstream");
    assert_eq!(
        response.json::<Value>().await.unwrap()["estimated"],
        true,
        "a proxy's estimate must not become an exact count"
    );
    assert_eq!(env.mock.calls.lock().unwrap()[0].2["model"], "gpt-4o");
    assert_eq!(
        env.mock.calls.lock().unwrap()[0].2["conversation"],
        json!({"id":"conv-1"})
    );
    *env.mock.source.lock().unwrap() = "local_estimate";
    env.mock.set(
        200,
        &json!({"object":"response.input_tokens","input_tokens":50,"estimated":false}),
    );
    let response = env.post(&env.body()).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-okapi-token-count-source"], "upstream");
    assert_eq!(response.json::<Value>().await.unwrap()["estimated"], true);
    sqlx::query("UPDATE api_keys SET status=2 WHERE id=$1")
        .bind(env.key_id)
        .execute(&env.pg)
        .await
        .unwrap();
    env.flush_auth().await;
    assert_eq!(env.post(&env.body()).await.status(), 401);
    assert_eq!(env.mock.count(), 2);
    env.assert_no_billing().await;
}
