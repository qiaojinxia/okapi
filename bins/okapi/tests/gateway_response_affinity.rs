//! 真实 PG/Redis + 两个独立模拟上游：从网关返回的 ID 发起续聊，不预填绑定。
#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{StatusCode, Uri},
    response::IntoResponse,
};
use fred::interfaces::{KeysInterface, ListInterface};
use okapi::{gateway, gateway::sched_redis::response_affinity::ResponseBinding};
use okapi_domain::Money;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone, Default)]
struct Mock {
    calls: Calls,
    failure: Arc<AtomicU16>,
    next_id: Arc<Mutex<Option<String>>>,
    stream_case: Arc<AtomicU16>,
}

async fn upstream(State(mock): State<Mock>, uri: Uri, body: Bytes) -> axum::response::Response {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let path = uri.path().to_owned();
    mock.calls
        .lock()
        .unwrap()
        .push((path.clone(), request.clone()));
    let failure = mock.failure.load(Ordering::SeqCst);
    if path.starts_with("/a/") && failure != 0 {
        return (
            StatusCode::from_u16(failure).unwrap(),
            axum::Json(json!({"error":{"code":"fixture_failure"}})),
        )
            .into_response();
    }
    let usage = json!({"input_tokens":100,"output_tokens":20});
    if path.ends_with("/input_tokens") {
        return axum::Json(json!({"object":"response.input_tokens","input_tokens":100}))
            .into_response();
    }
    if path.ends_with("/compact") {
        return axum::Json(json!({"id":"cmp-fixture","object":"response.compaction","output":[{"type":"compaction","encrypted_content":"opaque"}],"usage":usage})).into_response();
    }
    assert!(path.ends_with("/responses"), "unexpected fallback: {path}");
    let id = mock
        .next_id
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple()));
    let response = json!({"id":id,"object":"response","status":"completed","model":request["model"],
        "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}],"usage":usage});
    if request["stream"] == true {
        let mut events = vec![
            json!({"type":"response.created","response":{"object":"response","id":id,"status":"in_progress"}}),
            json!({"type":"response.output_text.delta","delta":"hello"}),
            json!({"type":"response.completed","response":response}),
        ];
        match mock.stream_case.load(Ordering::SeqCst) {
            1 => {
                events.remove(0);
            }
            2 => {
                events[2]["response"]["id"] = json!(format!("resp_changed_{id}"));
            }
            _ => {}
        }
        let mut stream = String::new();
        for event in &events {
            write!(
                stream,
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
            .unwrap();
        }
        ([("content-type", "text/event-stream")], stream).into_response()
    } else {
        axum::Json(response).into_response()
    }
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

struct Env {
    state: gateway::state::AppState,
    redis: fred::clients::Client,
    address: SocketAddr,
    mock: Mock,
    model: String,
    token: String,
    user: i64,
    key: i64,
    channel: i64,
    channel_key: i64,
    secondary: i64,
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let redis_client = okapi_store::connect_redis(&redis).await.unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let tag = Uuid::new_v4().simple().to_string();
    let user = okapi_store::provision::create_user(&pg, &format!("affinity-{tag}"))
        .await
        .unwrap();
    let token = format!("sk-affinity-{tag}");
    let key = okapi_store::provision::create_api_key(&pg, user, &hash(&token), "affinity")
        .await
        .unwrap();
    let model = format!("affinity-{tag}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let mock = Mock::default();
    let upstream = serve(Router::new().fallback(upstream).with_state(mock.clone())).await;
    let (channel, channel_key) = okapi_store::provision::create_channel(
        &pg,
        &format!("affinity-a-{tag}"),
        "openai",
        &format!("http://{upstream}/a"),
        "credential-a",
        &[&model],
        false,
        None,
    )
    .await
    .unwrap();
    let (secondary, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("affinity-b-{tag}"),
        "openai",
        &format!("http://{upstream}/b"),
        "credential-b",
        &[&model],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query(r#"UPDATE channels SET priority=CASE WHEN id=$1 THEN 100 ELSE 1 END, retry_policy='{"same_key_retries":0}'::jsonb WHERE id=ANY($2)"#)
        .bind(channel).bind(vec![channel, secondary]).execute(&pg).await.unwrap();
    published_pricing::publish(&pg, user).await;
    let state = gateway::build_state(&database, &redis, "affinity-test", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let address = serve(gateway::router(state.clone())).await;
    Env {
        redis: redis_client,
        state,
        address,
        mock,
        model,
        token,
        user,
        key,
        channel,
        channel_key,
        secondary,
    }
}

impl Env {
    async fn post_as(&self, token: &str, path: &str, body: &Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("http://{}{path}", self.address))
            .bearer_auth(token)
            .header("x-session-id", Uuid::new_v4().to_string())
            .json(body)
            .send()
            .await
            .unwrap()
    }
    async fn post(&self, path: &str, body: &Value) -> reqwest::Response {
        self.post_as(&self.token, path, body).await
    }
    fn body(&self, previous: Option<&str>) -> Value {
        json!({"model":self.model,"input":"continue","max_output_tokens":64,"previous_response_id":previous})
    }
    async fn first(&self, stream: bool) -> String {
        let mut body = self.body(None);
        body["stream"] = json!(stream);
        let response = self.post("/v1/responses", &body).await;
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
        let id = if stream {
            let raw = response.text().await.unwrap();
            let event: Value = raw
                .lines()
                .filter_map(|l| l.strip_prefix("data: "))
                .filter_map(|v| serde_json::from_str::<Value>(v).ok())
                .find(|v| v["type"] == "response.completed")
                .unwrap();
            event["response"]["id"].as_str().unwrap().to_owned()
        } else {
            response.json::<Value>().await.unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert!(
            self.state
                .sched
                .response_binding_get(self.user, self.key, &id)
                .await
                .unwrap()
                .is_some(),
            "ID must be bound before delivery"
        );
        self.idle().await;
        id
    }
    async fn idle(&self) {
        self.state
            .settlements
            .wait_idle(Duration::from_secs(5))
            .await;
        assert_eq!(self.state.settlements.in_flight(), 0);
    }
    fn calls(&self) -> Vec<(String, Value)> {
        self.mock.calls.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.mock.calls.lock().unwrap().clear();
    }
    async fn balance(&self) -> i64 {
        self.state
            .ledger
            .balance(self.user)
            .await
            .unwrap()
            .as_micros()
    }
    fn redis_key(&self, id: &str) -> String {
        format!("stick:resp:{{{}}}:v2:{}:{}", self.user, self.key, hash(id))
    }
}

#[tokio::test]
async fn json_and_sse_ids_pin_account_across_processes_and_priority_changes() {
    for stream in [false, true] {
        let mut env = setup().await;
        let previous = env.first(stream).await;
        assert!(env.calls()[0].0.starts_with("/a/"));
        sqlx::query("UPDATE channels SET priority=1000 WHERE id=$1")
            .bind(env.secondary)
            .execute(&env.state.pg)
            .await
            .unwrap();
        // 新 AppState 没有进程缓存；证明跨网关实例共享的历史绑定。
        let state = gateway::build_state(
            &std::env::var("DATABASE_URL").unwrap(),
            &std::env::var("OKAPI_REDIS_URL").unwrap(),
            "affinity-second-node",
            None,
            None,
        )
        .await
        .unwrap();
        env.address = serve(gateway::router(state.clone())).await;
        env.state = state;
        env.clear();
        let mut body = env.body(Some(&previous));
        body["stream"] = json!(stream);
        let response = env.post("/v1/responses", &body).await;
        assert_eq!(response.status(), 200);
        let raw = response.text().await.unwrap();
        assert!(raw.contains("hello"));
        env.idle().await;
        assert_eq!(env.calls().len(), 1);
        assert_eq!(env.calls()[0].0, "/a/responses");
        assert_eq!(env.calls()[0].1["previous_response_id"], previous);
        let layers: Vec<i16> = sqlx::query_scalar(
            "SELECT sticky_layer FROM billing_records WHERE user_id=$1 AND status=20 ORDER BY id",
        )
        .bind(env.user)
        .fetch_all(&env.state.pg)
        .await
        .unwrap();
        assert_eq!(layers.last(), Some(&1));
    }
}

#[tokio::test]
async fn unknown_other_user_other_key_expired_and_invalid_ids_never_reach_upstream() {
    let env = setup().await;
    let previous = env.first(false).await;
    let other_token = format!("sk-other-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(&env.state.pg, env.user, &hash(&other_token), "other")
        .await
        .unwrap();
    let foreign =
        okapi_store::provision::create_user(&env.state.pg, &format!("foreign-{}", Uuid::new_v4()))
            .await
            .unwrap();
    let foreign_token = format!("sk-foreign-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(
        &env.state.pg,
        foreign,
        &hash(&foreign_token),
        "foreign",
    )
    .await
    .unwrap();
    env.clear();
    for path in [
        "/v1/responses",
        "/v1/responses/compact",
        "/v1/responses/input_tokens",
    ] {
        for (token, id) in [
            (&env.token, "resp_unknown"),
            (&other_token, previous.as_str()),
            (&foreign_token, previous.as_str()),
        ] {
            let response = env.post_as(token, path, &env.body(Some(id))).await;
            assert_eq!(response.status(), 404);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"]["param"],
                "previous_response_id"
            );
        }
        for invalid in [
            json!(""),
            json!(17),
            json!("a".repeat(513)),
            json!("white space"),
        ] {
            let mut body = env.body(None);
            body["previous_response_id"] = invalid;
            assert_eq!(env.post(path, &body).await.status(), 400);
        }
        let mut body = env.body(Some(&previous));
        body["conversation"] = json!("conv_conflict");
        assert_eq!(env.post(path, &body).await.status(), 400);
    }
    let _: bool = env
        .redis
        .pexpire(env.redis_key(&previous), 1, None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(
        env.post("/v1/responses", &env.body(Some(&previous)))
            .await
            .status(),
        404
    );
    assert!(env.calls().is_empty());
}

#[tokio::test]
async fn continuation_revalidates_live_key_channel_pool_and_credential() {
    let env = setup().await;
    let previous = env.first(false).await;
    let before = env.balance().await;
    let pg = &env.state.pg;
    // 不清进程候选缓存：续聊必须直接验证现状。
    let mutations = [
        (
            "UPDATE channel_keys SET status=5 WHERE id=$1",
            "UPDATE channel_keys SET status=1 WHERE id=$1",
            env.channel_key,
        ),
        (
            "UPDATE channels SET status=2 WHERE id=$1",
            "UPDATE channels SET status=1 WHERE id=$1",
            env.channel,
        ),
        (
            "UPDATE channels SET deleted_at=now() WHERE id=$1",
            "UPDATE channels SET deleted_at=NULL WHERE id=$1",
            env.channel,
        ),
        (
            "UPDATE channel_keys SET credential_ciphertext='changed-account' WHERE id=$1",
            "UPDATE channel_keys SET credential_ciphertext='credential-a' WHERE id=$1",
            env.channel_key,
        ),
        (
            r#"UPDATE channels SET settings='{"responses_native":false}' WHERE id=$1"#,
            "UPDATE channels SET settings='{}' WHERE id=$1",
            env.channel,
        ),
        (
            "DELETE FROM pool_channels WHERE channel_id=$1",
            "INSERT INTO pool_channels(pool_code,channel_id) VALUES('default',$1)",
            env.channel,
        ),
    ];
    for (change, restore, id) in mutations {
        sqlx::query(change).bind(id).execute(pg).await.unwrap();
        env.clear();
        for path in [
            "/v1/responses",
            "/v1/responses/compact",
            "/v1/responses/input_tokens",
        ] {
            let response = env.post(path, &env.body(Some(&previous))).await;
            assert_eq!(
                response.status(),
                503,
                "{change} {path}: {}",
                response.text().await.unwrap()
            );
        }
        env.idle().await;
        assert!(env.calls().is_empty(), "{change}");
        assert_eq!(env.balance().await, before);
        sqlx::query(restore).bind(id).execute(pg).await.unwrap();
    }
}

#[tokio::test]
async fn bound_failure_never_switches_account_downgrades_protocol_or_charges() {
    let env = setup().await;
    let previous = env.first(false).await;
    let before = env.balance().await;
    for status in [404, 405, 429, 500, 401] {
        sqlx::query("UPDATE channel_keys SET status=1,cooldown_until=NULL WHERE id=$1")
            .bind(env.channel_key)
            .execute(&env.state.pg)
            .await
            .unwrap();
        env.mock.failure.store(status, Ordering::SeqCst);
        env.clear();
        let response = env.post("/v1/responses", &env.body(Some(&previous))).await;
        assert!(!response.status().is_success());
        env.idle().await;
        assert_eq!(env.calls().len(), 1, "status {status}");
        assert_eq!(env.calls()[0].0, "/a/responses");
        assert_eq!(env.balance().await, before);
        assert!(
            env.state
                .ledger
                .list_reservations(env.user)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn compact_count_and_channel_field_mutations_preserve_the_same_parent() {
    let env = setup().await;
    let previous = env.first(true).await;
    sqlx::query("UPDATE channels SET priority=1000 WHERE id=$1")
        .bind(env.secondary)
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1").bind(env.channel)
        .bind(json!({"strip_request_fields":["previous_response_id"], "inject_request_fields":{"previous_response_id":"resp_forged","conversation":"conv_forged"}})).execute(&env.state.pg).await.unwrap();
    env.clear();
    let before = env.balance().await;
    let count = env
        .post("/v1/responses/input_tokens", &env.body(Some(&previous)))
        .await;
    assert_eq!(count.status(), 200);
    assert_eq!(count.json::<Value>().await.unwrap()["input_tokens"], 100);
    assert_eq!(env.balance().await, before);
    let compact = env
        .post("/v1/responses/compact", &env.body(Some(&previous)))
        .await;
    assert_eq!(compact.status(), 200, "{}", compact.text().await.unwrap());
    assert_eq!(
        compact.json::<Value>().await.unwrap()["object"],
        "response.compaction"
    );
    env.idle().await;
    assert_eq!(env.calls().len(), 2);
    for (path, body) in env.calls() {
        assert!(path.starts_with("/a/"));
        assert_eq!(body["previous_response_id"], previous);
        assert!(body.get("conversation").is_none());
    }
    env.mock.failure.store(404, Ordering::SeqCst);
    env.clear();
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/responses/input_tokens", env.address))
        .bearer_auth(&env.token)
        .header("x-okapi-token-count-mode", "auto")
        .json(&env.body(Some(&previous)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 501);
    assert_eq!(env.calls().len(), 1);
}

#[tokio::test]
async fn binding_corruption_write_failure_and_collision_fail_closed() {
    let env = setup().await;
    let previous = env.first(false).await;
    let binding = env
        .state
        .sched
        .response_binding_get(env.user, env.key, &previous)
        .await
        .unwrap()
        .unwrap();
    let mut other = binding.clone();
    other.channel_key_id += 1;
    assert!(
        env.state
            .sched
            .response_binding_set(env.user, env.key, &previous, &other)
            .await
            .is_err()
    );
    assert_eq!(
        env.state
            .sched
            .response_binding_get(env.user, env.key, &previous)
            .await
            .unwrap(),
        Some(binding)
    );
    let _: () = env
        .redis
        .set(
            env.redis_key(&previous),
            "invalid-binding",
            None,
            None,
            false,
        )
        .await
        .unwrap();
    env.clear();
    assert_eq!(
        env.post("/v1/responses", &env.body(Some(&previous)))
            .await
            .status(),
        503
    );
    assert!(env.calls().is_empty());
    let before = env.balance().await;
    for stream in [false, true] {
        let id = format!("resp_{}", Uuid::new_v4().simple());
        let _: i64 = env
            .redis
            .lpush(env.redis_key(&id), "wrongtype")
            .await
            .unwrap();
        *env.mock.next_id.lock().unwrap() = Some(id.clone());
        let mut body = env.body(None);
        body["stream"] = json!(stream);
        env.clear();
        let response = env.post("/v1/responses", &body).await;
        assert_eq!(response.status(), 503);
        assert!(!response.text().await.unwrap().contains(&id));
        env.idle().await;
        assert_eq!(env.calls().len(), 1);
        assert_eq!(env.balance().await, before);
        let _: i64 = env.redis.del(env.redis_key(&id)).await.unwrap();
    }
}

#[tokio::test]
async fn oauth_refresh_keeps_account_identity_but_account_and_endpoint_changes_do_not() {
    let env = setup().await;
    let mut candidate =
        okapi_store::channels::candidates_for_model(&env.state.pg, &env.model, &["default"], None)
            .await
            .unwrap()
            .remove(0);
    candidate.provider = "codex".to_owned();
    let mut cred = okapi_store::credential::OAuthCredential {
        access_token: "access-one".to_owned(),
        refresh_token: "refresh-one".to_owned(),
        expires_at: 1,
        account_id: Some("account-one".to_owned()),
        account_label: None,
        scope: None,
    };
    candidate.credential = cred.to_plaintext();
    let original = ResponseBinding::from_candidate(&candidate);
    cred.access_token = "access-two".to_owned();
    cred.refresh_token = "refresh-two".to_owned();
    cred.expires_at = 9999;
    candidate.credential = cred.to_plaintext();
    assert!(original.matches(&candidate));
    cred.account_id = Some("account-two".to_owned());
    candidate.credential = cred.to_plaintext();
    assert!(!original.matches(&candidate));
    cred.account_id = Some("account-one".to_owned());
    candidate.credential = cred.to_plaintext();
    candidate.api_base = Some("https://different.example/v1".to_owned());
    assert!(!original.matches(&candidate));
}

#[tokio::test]
async fn late_stream_binding_failure_and_changed_id_stop_without_losing_usage() {
    for case in [1, 2] {
        let env = setup().await;
        let id = format!("resp_{}", Uuid::new_v4().simple());
        *env.mock.next_id.lock().unwrap() = Some(id.clone());
        env.mock.stream_case.store(case, Ordering::SeqCst);
        if case == 1 {
            let _: i64 = env
                .redis
                .lpush(env.redis_key(&id), "wrongtype")
                .await
                .unwrap();
        }
        let before = env.balance().await;
        let mut body = env.body(None);
        body["stream"] = json!(true);
        let response = env.post("/v1/responses", &body).await;
        assert_eq!(response.status(), 200, "首字发送后的错误用流事件报告");
        let raw = response.text().await.unwrap();
        assert!(raw.contains("hello"), "{raw}");
        assert!(raw.contains("event: error"), "{raw}");
        assert!(!raw.contains("event: response.completed"), "{raw}");
        let error: Value = raw
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["type"] == "error")
            .unwrap();
        assert_eq!(
            error["param"],
            if case == 1 {
                "response_binding"
            } else {
                "response_id_changed"
            }
        );
        env.idle().await;
        assert_eq!(env.calls().len(), 1);
        assert_eq!(env.balance().await, before - 240, "流中断仍结算真实产出");
        assert!(
            env.state
                .ledger
                .list_reservations(env.user)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            env.state
                .sched
                .response_binding_get(env.user, env.key, &format!("resp_changed_{id}"))
                .await
                .unwrap()
                .is_none()
        );
        let _: i64 = env.redis.del(env.redis_key(&id)).await.unwrap();
    }
}

#[tokio::test]
async fn history_never_uses_model_fallback_or_bypasses_channel_limits() {
    let mut env = setup().await;
    let previous = env.first(false).await;
    let fallback = format!("fallback-{}", Uuid::new_v4());
    okapi_store::provision::create_model_ratio(&env.state.pg, &fallback, "1", "1", "1")
        .await
        .unwrap();
    sqlx::query("UPDATE models SET fallback_models=$2 WHERE model_name=$1")
        .bind(&env.model)
        .bind(json!([fallback]))
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET models=$2 WHERE id=$1")
        .bind(env.secondary)
        .bind(json!([env.model, fallback]))
        .execute(&env.state.pg)
        .await
        .unwrap();
    published_pricing::publish(&env.state.pg, env.user).await;
    let state = gateway::build_state(
        &std::env::var("DATABASE_URL").unwrap(),
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "affinity-fallback-node",
        None,
        None,
    )
    .await
    .unwrap();
    env.address = serve(gateway::router(state.clone())).await;
    env.state = state;
    // 先证明备选模型确实可调且已定价，避免测试只因缺价而未发生降级。
    let mut direct = env.body(None);
    direct["model"] = json!(fallback);
    let reply = env.post("/v1/responses", &direct).await;
    assert_eq!(reply.status(), 200);
    let _ = reply.text().await.unwrap();
    env.idle().await;
    // 原 key 并发满；另一账号、另一模型均可用，也不能转发过去。
    sqlx::query("UPDATE channel_keys SET max_concurrency=1 WHERE id=$1")
        .bind(env.channel_key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let occupied = okapi::gateway::sched_redis::channel_permit::ChannelPermit::acquire_key(
        &env.state.sched,
        env.channel_key,
        Some(1),
    )
    .await
    .unwrap()
    .unwrap();
    env.clear();
    let before = env.balance().await;
    assert_eq!(
        env.post("/v1/responses", &env.body(Some(&previous)))
            .await
            .status(),
        503
    );
    assert_eq!(
        env.post("/v1/responses/input_tokens", &env.body(Some(&previous)))
            .await
            .status(),
        429
    );
    occupied.release().await;
    env.idle().await;
    assert!(env.calls().is_empty());
    assert_eq!(env.balance().await, before);
    // 当前模型白名单拒绝优先于历史路由。
    sqlx::query("UPDATE api_keys SET model_allowlist=$2 WHERE id=$1")
        .bind(env.key)
        .bind(json!([fallback]))
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state.sched.auth_del(&hash(&env.token)).await;
    assert_eq!(
        env.post("/v1/responses", &env.body(Some(&previous)))
            .await
            .status(),
        403
    );
    assert!(env.calls().is_empty());
}

#[tokio::test]
async fn disconnected_binding_storage_has_bounded_fail_closed_reads_and_writes() {
    let client = fred::types::Builder::default_centralized().build().unwrap();
    // 不启动连接，模拟存储不可用；由接口自身超时界限结束。
    let sched = gateway::sched_redis::SchedulerRedis::new(client);
    let started = std::time::Instant::now();
    assert_eq!(
        sched
            .response_binding_get(1, 1, "resp_offline")
            .await
            .unwrap_err()
            .status,
        503
    );
    let binding = ResponseBinding {
        channel_id: 1,
        channel_key_id: 1,
        identity: "fixture".to_owned(),
    };
    assert_eq!(
        sched
            .response_binding_set(1, 1, "resp_offline", &binding)
            .await
            .unwrap_err()
            .status,
        503
    );
    assert!(started.elapsed() < Duration::from_secs(6));
}
