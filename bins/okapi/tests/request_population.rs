//! One real gateway call and HTTP refund must retain call and measurement populations.

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::worker::chsink;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    use std::fmt::Write as _;
    let chunks = [
        json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
        json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":0}}}),
    ];
    let mut body = String::new();
    for c in chunks {
        let _ = write!(body, "data: {c}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

struct Env {
    pg: PgPool,
    state: gateway::state::AppState,
    console: SocketAddr,
    gateway: SocketAddr,
    model: String,
    super_token: String,
    user_id: i64,
    user_token: String,
}

async fn setup() -> Env {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("okapi=error")
        .try_init();
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-ops-{}", &suffix[..10]);

    // super_admin 与普通用户
    let super_id = okapi_store::provision::create_user(&pg, &format!("os-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-ops-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-ops-s")
        .await
        .unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("ou-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-ops-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-ops-u")
        .await
        .unwrap();

    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let mock = serve(Router::new().route("/ok/v1/chat/completions", post(mock_ok))).await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("ops-{suffix}"),
        "openai",
        &format!("http://{mock}/ok/v1"),
        "mock",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    published_pricing::publish(&pg, super_id).await;
    let mut state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    // Probe fixture: route every chart to its own CH database, never the global test data.
    let database = format!("okapi_request_population_{suffix}");
    state.ch = Some(okapi_store::ChClient::new(ch_url.as_deref().unwrap(), &database).unwrap());
    state.ch.as_ref().unwrap().ensure_schema().await.unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_000_000))
        .await
        .unwrap();
    okapi_ledger::pg::record_credit(
        &pg,
        user_id,
        Money::from_micros(1_000_000),
        "recharge",
        "test",
        json!({}),
    )
    .await
    .unwrap();

    let console = serve(console::router(state.clone())).await;
    let gw = serve(gateway::router(state.clone())).await;
    Env {
        pg,
        state,
        console,
        gateway: gw,
        model,
        super_token,
        user_id,
        user_token,
    }
}

async fn chat_settled(env: &Env) -> (Uuid, i64) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({
            "model": env.model, "stream": true, "max_tokens": 32,
            "messages": [{"role":"user","content":"hello ops"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let request_id = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap();
    let _ = resp.text().await.unwrap();
    for _ in 0..50 {
        if let Some(r) = sqlx::query!(
            r#"SELECT amount_micro FROM billing_records WHERE request_id = $1 AND status = 20"#,
            request_id
        )
        .fetch_optional(&env.pg)
        .await
        .unwrap()
        {
            return (request_id, r.amount_micro);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("结算未出现");
}

async fn mirror(env: &Env) {
    let ch = env.state.ch.as_ref().unwrap();
    for _ in 0..100 {
        if chsink::process_once(&env.pg, ch).await.unwrap() == 0 {
            return;
        }
    }
    panic!("probe outbox did not drain");
}

async fn chart(env: &Env, path: &str, token: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("http://{}{path}", env.console))
        .header("cache-control", "no-cache")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap();
    println!(
        "REQUEST_POPULATION_HTTP {}",
        json!({"path":path,"status":status,"body":body})
    );
    assert_eq!(status, 200, "{path}: {body}");
    body
}

fn cell(row: &Value, key: &str) -> i64 {
    row[key]
        .as_i64()
        .or_else(|| row[key].as_str().and_then(|v| v.parse().ok()))
        .unwrap()
}

async fn assert_refunded_analytics(env: &Env, trend_path: &str, trend_before: &Value) {
    let trend_after = chart(env, trend_path, &env.super_token).await;
    for field in [
        "requests",
        "errors",
        "tokens",
        "prompt_tokens",
        "completion_tokens",
        "error_rate_bp",
        "cache_hit_bp",
        "avg_latency_ms",
        "avg_ttft_ms",
        "avg_output_tps_milli",
    ] {
        assert_eq!(
            trend_after["total"][field], trend_before["total"][field],
            "analytics refund changed {field}"
        );
    }
    assert_eq!(trend_after["total"]["requests"], 1);
    assert_eq!(trend_after["total"]["tokens"], 120);
    assert_eq!(trend_after["total"]["amount_micro"], 0);
    let breakdown = chart(
        env,
        &format!(
            "/admin/stats/breakdown?days=1&user_id={}&by=model&cached=false",
            env.user_id
        ),
        &env.super_token,
    )
    .await;
    assert_eq!(breakdown["total_requests"], 1);
    assert_eq!(breakdown["total_tokens"], 120);
    assert_eq!(breakdown["total_amount_micro"], 0);
    assert_eq!(breakdown["data"][0]["requests"], 1);
    for (metric, expected) in [("requests", 1), ("tokens", 120), ("amount", 0)] {
        let flow = chart(
            env,
            &format!(
                "/admin/stats/flow?days=1&user_id={}&metric={metric}&cached=false",
                env.user_id
            ),
            &env.super_token,
        )
        .await;
        assert_eq!(flow["total"], expected, "flow {metric}");
        for stage in flow["stages"].as_array().unwrap() {
            let total: i64 = flow["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|node| &node["stage"] == stage)
                .map(|node| node["value"].as_i64().unwrap())
                .sum();
            assert_eq!(total, expected, "flow {metric} stage {stage}");
        }
    }
}

#[tokio::test]
async fn refunded_call_remains_one_api_request_on_charts() {
    let env = setup().await;
    let (request_id, amount) = chat_settled(&env).await;
    assert_eq!(amount, 240);
    mirror(&env).await;
    let before = chart(&env, "/api/me/stats/breakdown?days=1", &env.user_token).await;
    assert_eq!(before["total"]["requests"], 1);
    assert_eq!(before["total"]["tokens"], 120);
    assert_eq!(before["total"]["amount_micro"], 240);
    let trend_path = format!(
        "/admin/stats/trend?days=1&user_id={}&compare=false&cached=false",
        env.user_id
    );
    let trend_before = chart(&env, &trend_path, &env.super_token).await;
    assert_eq!(trend_before["total"]["requests"], 1);
    assert_eq!(trend_before["total"]["amount_micro"], 240);
    let refund = reqwest::Client::new()
        .post(format!("http://{}/admin/billing/refund", env.console))
        .bearer_auth(&env.super_token)
        .json(&json!({"request_id":request_id,"reason":"request population audit"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refund.status(), 200);
    let refund: Value = refund.json().await.unwrap();
    assert_eq!(refund["outcome"], "refunded");
    assert_eq!(refund["refunded_micro"], 240);
    mirror(&env).await;
    let after = chart(&env, "/api/me/stats/breakdown?days=1", &env.user_token).await;
    let overview = chart(&env, "/admin/stats/overview?days=1", &env.super_token).await;
    let activity = chart(&env, "/api/me/stats/activity", &env.user_token).await;
    let logs = chart(&env, "/api/me/logs/stat", &env.user_token).await;
    let admin_logs = chart(
        &env,
        &format!("/admin/logs/stat?user_id={}", env.user_id),
        &env.super_token,
    )
    .await;
    assert_eq!(admin_logs["requests"], 1);
    assert_eq!(admin_logs["financial_records"], 2);
    assert_eq!(admin_logs["tokens"], 120);
    assert_eq!(admin_logs["amount_micro"], 0);
    let ch = env.state.ch.as_ref().unwrap();
    let raw = ch.query_json_each_row(&format!(
        "SELECT count() AS events,uniqExact(request_id) AS request_ids,sum(prompt_tokens+completion_tokens) AS tokens,sum(amount_micro) AS amount FROM request_log_raw WHERE user_id={}", env.user_id
    )).await.unwrap();
    assert_eq!(cell(&raw[0], "events"), 2);
    assert_eq!(cell(&raw[0], "request_ids"), 1);
    assert_eq!(cell(&raw[0], "tokens"), 120);
    assert_eq!(cell(&raw[0], "amount"), 0);
    assert_eq!(after["total"]["tokens"], 120);
    assert_eq!(after["total"]["amount_micro"], 0);
    for field in [
        "cache_hit_bp",
        "measured_cache_hit_coverage_bp",
        "avg_rpm_micro",
        "avg_output_tps_milli",
        "avg_latency_ms",
        "avg_ttft_ms",
    ] {
        assert_eq!(
            after["total"][field], before["total"][field],
            "refund changed {field}"
        );
    }
    assert_eq!(after["total"]["input_units"]["complete"], true);
    assert_eq!(after["total"]["input_units"]["unknown_requests"], 0);
    assert_eq!(
        after["total"]["token_provenance"]["prompt"]["upstream"]["request_share_bp"],
        10_000
    );
    assert_refunded_analytics(&env, &trend_path, &trend_before).await;
    let report = json!({"scope":"One real gateway request, actual admin HTTP refund, PG outbox delivery and isolated CH chart APIs; not synthetic refund payload",
        "request_id":request_id,"user_id":env.user_id,"before":before,"after":after,
        "overview":overview,"activity":activity,"logs_stat":logs,"raw":raw});
    println!("REQUEST_POPULATION_PROBE {report}");
    ch.execute(&format!(
        "DROP DATABASE {} SYNC",
        ch.query_json_each_row("SELECT currentDatabase() AS db")
            .await
            .unwrap()[0]["db"]
            .as_str()
            .unwrap()
    ))
    .await
    .unwrap();
    assert_eq!(
        after["total"]["requests"], 1,
        "one API call plus its financial refund must remain one request"
    );
    assert_eq!(overview["today"]["requests"], 1);
}
