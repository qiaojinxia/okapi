//! M1 验收集成测试（IMPLEMENTATION §13 M1）：
//! 流式计费闭环 / 空回复不计费 / 断流 failover / 负余额拒绝 / cache 计费与透传。
//! 依赖 .env 中的 DATABASE_URL 与 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

// ---- mock 上游 ----

fn sse_body(chunks: &[Value], done: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for chunk in chunks {
        let _ = write!(out, "data: {chunk}\n\n");
    }
    if done {
        out.push_str("data: [DONE]\n\n");
    }
    out
}

fn content_chunk(text: &str) -> Value {
    json!({"id":"c","object":"chat.completion.chunk",
        "choices":[{"index":0,"delta":{"content":text}}]})
}

fn role_chunk() -> Value {
    json!({"id":"c","object":"chat.completion.chunk",
        "choices":[{"index":0,"delta":{"role":"assistant"}}]})
}

fn usage_chunk(prompt: u32, cached: u32, completion: u32) -> Value {
    json!({"id":"c","object":"chat.completion.chunk","choices":[],
        "usage":{"prompt_tokens":prompt,"completion_tokens":completion,
                 "prompt_tokens_details":{"cached_tokens":cached}}})
}

fn sse_response(body: String) -> axum::response::Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    if req["stream"].as_bool().unwrap_or(false) {
        sse_response(sse_body(
            &[
                role_chunk(),
                content_chunk("Hello"),
                content_chunk(" world"),
                usage_chunk(100, 0, 20),
            ],
            true,
        ))
    } else {
        axum::Json(json!({
            "id":"cmpl","object":"chat.completion",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Hello world"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,
                     "prompt_tokens_details":{"cached_tokens":0}}
        }))
        .into_response()
    }
}

async fn mock_cache(_body: axum::body::Bytes) -> axum::response::Response {
    sse_response(sse_body(
        &[
            role_chunk(),
            content_chunk("cached reply"),
            usage_chunk(1000, 800, 50),
        ],
        true,
    ))
}

async fn mock_empty(_body: axum::body::Bytes) -> axum::response::Response {
    sse_response(sse_body(&[role_chunk()], true))
}

async fn mock_fail(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"error":{"message":"boom"}})),
    )
        .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/v1/chat/completions", post(mock_ok))
        .route("/cache/v1/chat/completions", post(mock_cache))
        .route("/empty/v1/chat/completions", post(mock_empty))
        .route("/fail/v1/chat/completions", post(mock_fail));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

// ---- 测试环境 ----

struct TestEnv {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

/// channels: (base_path 如 "/ok/v1", priority)
async fn setup(pricing: (&str, &str, &str), balance: Money, channels: &[(&str, i32)]) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);

    // 先种数据再 build_state（PriceBook 启动装载）
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-test-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, pricing.0, pricing.1, pricing.2)
        .await
        .unwrap();

    let mock = spawn_mock().await;
    for (i, (path, priority)) in channels.iter().enumerate() {
        let base = format!("http://{mock}{path}");
        let (channel_id, _key) = okapi_store::provision::create_channel(
            &pg,
            &format!("ch{i}-{suffix}"),
            "openai",
            &base,
            "mock-credential",
            &[model.as_str()],
            false,
            None,
        )
        .await
        .unwrap();
        sqlx::query!(
            "UPDATE channels SET priority = $2 WHERE id = $1",
            channel_id,
            priority
        )
        .execute(&pg)
        .await
        .unwrap();
    }

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    if !balance.is_zero() {
        state.ledger.credit(user_id, balance).await.unwrap();
    }
    let ledger = state.ledger.clone();

    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        ledger,
        gateway: addr,
        token,
        user_id,
        model,
    }
}

fn chat_body(env: &TestEnv, stream: bool) -> Value {
    json!({
        "model": env.model,
        "stream": stream,
        "max_tokens": 64,
        "messages": [{"role":"user","content":"hi there"}]
    })
}

async fn post_chat(env: &TestEnv, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&chat_body(env, stream))
        .send()
        .await
        .unwrap()
}

#[derive(Debug)]
struct RecordRow {
    status: i16,
    log_type: i16,
    amount_micro: i64,
    cached_tokens: i32,
    failover_count: i16,
    error_code: Option<String>,
    pricing_snapshot: Option<Value>,
}

async fn wait_record(pg: &PgPool, request_id: Uuid) -> RecordRow {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, log_type, amount_micro, cached_tokens, failover_count,
                      error_code, pricing_snapshot
               FROM billing_records WHERE request_id = $1"#,
            request_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return RecordRow {
                status: r.status,
                log_type: r.log_type,
                amount_micro: r.amount_micro,
                cached_tokens: r.cached_tokens,
                failover_count: r.failover_count,
                error_code: r.error_code,
                pricing_snapshot: r.pricing_snapshot,
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("billing_records 未在 5s 内出现 request_id={request_id}");
}

fn request_id_of(resp: &reqwest::Response) -> Uuid {
    resp.headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .expect("响应缺少 x-okapi-request-id")
}

// ---- 用例 ----

/// 流式闭环：透传 + usage 结算 + 余额扣减（M1 主验收）。
#[tokio::test]
async fn stream_happy_path_bills_exactly() {
    let env = setup(
        ("1", "1", "1"),
        Money::from_micros(10_000_000),
        &[("/ok/v1", 0)],
    )
    .await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200);
    let request_id = request_id_of(&resp);
    let text = resp.text().await.unwrap();
    assert!(text.contains("Hello"), "{text}");
    assert!(text.contains("[DONE]"), "{text}");

    let rec = wait_record(&env.pg, request_id).await;
    // (100 + 20×1) × ratio1 × $2/1M = 240 micro
    assert_eq!(rec.amount_micro, 240);
    assert_eq!(rec.status, 20, "committed");
    assert_eq!(rec.log_type, 2);
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 10_000_000 - 240);
}

/// 空回复不计费：干净错误 + 全额退款（§3.7-2）。
#[tokio::test]
async fn empty_completion_is_free_and_clean_error() {
    let env = setup(
        ("1", "1", "1"),
        Money::from_micros(10_000_000),
        &[("/empty/v1", 0)],
    )
    .await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 502);
    let request_id = request_id_of(&resp);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "empty_completion");

    let rec = wait_record(&env.pg, request_id).await;
    assert_eq!(rec.amount_micro, 0);
    assert_eq!(rec.status, 40, "failed");
    assert_eq!(rec.log_type, 5, "错误日志");
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 10_000_000, "余额分毫未动");
}

/// 首字前失败 failover：客户端无感拿到完整流，failover_count=1。
#[tokio::test]
async fn failover_before_first_token_is_seamless() {
    let env = setup(
        ("1", "1", "1"),
        Money::from_micros(10_000_000),
        &[("/fail/v1", 10), ("/ok/v1", 0)],
    )
    .await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200, "对客户端必须无感");
    let request_id = request_id_of(&resp);
    let text = resp.text().await.unwrap();
    assert!(text.contains("Hello"));

    let rec = wait_record(&env.pg, request_id).await;
    assert_eq!(rec.status, 20);
    assert_eq!(rec.failover_count, 1);
    assert_eq!(rec.amount_micro, 240);
}

/// 余额不足：reserve 阶段拒绝（fail-closed）。
#[tokio::test]
async fn insufficient_balance_rejected_at_reserve() {
    let env = setup(("1", "1", "1"), Money::ZERO, &[("/ok/v1", 0)]).await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 429);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "insufficient_quota");
}

/// cache 计费：cached tokens 按 cache_ratio 打折，快照可解释（对拍 fixtures 同式）。
#[tokio::test]
async fn cached_tokens_billed_with_cache_ratio() {
    let env = setup(
        ("1.25", "4", "0.5"),
        Money::from_micros(10_000_000),
        &[("/cache/v1", 0)],
    )
    .await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200);
    let request_id = request_id_of(&resp);
    let _ = resp.text().await.unwrap();

    let rec = wait_record(&env.pg, request_id).await;
    // (200×1 + 800×0.5 + 50×4) × 1.25 × $2/1M = 2000 micro
    assert_eq!(rec.amount_micro, 2000);
    assert_eq!(rec.cached_tokens, 800);
    let snapshot = rec.pricing_snapshot.expect("必须携带 pricing_snapshot");
    assert_eq!(snapshot["cache_ratio"], json!(0.5));
    assert_eq!(snapshot["model_ratio"], json!(1.25));
}

/// 非流式闭环：JSON 透传 + 结算。
#[tokio::test]
async fn non_stream_passthrough_and_billing() {
    let env = setup(
        ("1", "1", "1"),
        Money::from_micros(10_000_000),
        &[("/ok/v1", 0)],
    )
    .await;
    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let request_id = request_id_of(&resp);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello world");

    let rec = wait_record(&env.pg, request_id).await;
    assert_eq!(rec.amount_micro, 240);
    assert_eq!(rec.status, 20);
    assert!(rec.error_code.is_none());
}
