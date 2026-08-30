//! M3 anthropic 上游验收：OpenAI 协议客户端 → anthropic 渠道全链路。
//! 覆盖：请求出向转换（含必填 max_tokens 与 x-api-key 头）、SSE 事件流回转
//! OpenAI chunk、cache usage 映射计费、非流式回转。
//! 依赖 .env 的 DATABASE_URL / OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::net::SocketAddr;
use uuid::Uuid;

// ---- mock anthropic 上游 ----

fn sse(events: &[(&str, Value)]) -> String {
    let mut out = String::new();
    for (event, data) in events {
        let _ = write!(out, "event: {event}\ndata: {data}\n\n");
    }
    out
}

fn anthropic_stream_body() -> String {
    sse(&[
        (
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_up","model":"claude-real",
                "usage":{"input_tokens":100,"cache_read_input_tokens":800,
                         "cache_creation_input_tokens":0,"output_tokens":1}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"Hello"}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":" anthropic"}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                "usage":{"output_tokens":50}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ])
}

async fn mock_messages(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    // 协议断言：x-api-key + anthropic-version + 必填 max_tokens + system 抽取
    assert_eq!(
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        Some("mock-credential"),
        "anthropic 凭证必须走 x-api-key"
    );
    assert!(headers.get("anthropic-version").is_some());
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert!(req["max_tokens"].as_u64().unwrap() > 0, "max_tokens 必填");
    assert_eq!(req["system"], "sys prompt");
    assert_eq!(req["messages"][0]["role"], "user");
    assert!(
        req.get("model")
            .and_then(Value::as_str)
            .unwrap()
            .starts_with("m-"),
        "model 字段应为映射后的上游名"
    );

    if req["stream"].as_bool().unwrap_or(false) {
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            anthropic_stream_body(),
        )
            .into_response()
    } else {
        axum::Json(json!({
            "id":"msg_json","type":"message","role":"assistant","model":"claude-real",
            "content":[{"type":"text","text":"Hello anthropic"}],
            "stop_reason":"end_turn",
            "usage":{"input_tokens":100,"output_tokens":50,
                     "cache_read_input_tokens":800,"cache_creation_input_tokens":0}
        }))
        .into_response()
    }
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new().route("/v1/messages", post(mock_messages));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

// ---- 环境 ----

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);

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
    okapi_store::provision::create_model_ratio(&pg, &model, "500", "2", "0.5")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("anthropic-{suffix}"),
        "anthropic",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();

    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        gateway: addr,
        token,
        user_id,
        model,
    }
}

async fn post_chat(env: &TestEnv, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "stream": stream,
            "max_tokens": 256,
            "messages": [
                {"role": "system", "content": "sys prompt"},
                {"role": "user", "content": "hi there"}
            ]
        }))
        .send()
        .await
        .unwrap()
}

async fn wait_record(pg: &PgPool, user_id: i64) -> (i16, i64, i32, i32) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens, cached_tokens
               FROM billing_records WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.status, r.amount_micro, r.prompt_tokens, r.cached_tokens);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

/// 流式：anthropic 事件流回转 OpenAI chunk；cache usage 进结算。
#[tokio::test]
async fn anthropic_stream_end_to_end() {
    let env = setup().await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/event-stream")
    );
    let text = resp.text().await.unwrap();

    // 客户端看到的必须是 OpenAI chunk 形状
    let chunks: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| serde_json::from_str(d).ok())
        .collect();
    assert!(text.contains("data: [DONE]"), "必须以 [DONE] 收尾");
    let content: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(content, "Hello anthropic");
    assert!(
        chunks
            .iter()
            .all(|c| c["object"] == "chat.completion.chunk"),
        "所有 chunk 必须是 OpenAI 形状"
    );
    let usage = chunks
        .iter()
        .find_map(|c| c.get("usage").filter(|u| !u.is_null()))
        .expect("必须有 usage chunk");
    assert_eq!(usage["prompt_tokens"], 900, "100 input + 800 cache_read");
    assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], 800);
    assert_eq!(usage["completion_tokens"], 50);

    // (100 非缓存×1 + 800 缓存×0.5 + 50 补全×2) × model_ratio 500 × $2/1M
    // = 600 × 500 × 2 = 600_000 micro
    let (status, amount, prompt_tokens, cached_tokens) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20, "committed");
    assert_eq!(prompt_tokens, 900);
    assert_eq!(cached_tokens, 800);
    assert_eq!(amount, 600_000, "cache 半价计费必须生效");
}

/// 非流式：anthropic JSON 回转 OpenAI chat.completion。
#[tokio::test]
async fn anthropic_json_end_to_end() {
    let env = setup().await;
    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello anthropic");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["prompt_tokens"], 900);

    let (status, amount, _, cached) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(cached, 800);
    assert_eq!(amount, 600_000);
}
