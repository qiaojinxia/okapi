//! M3 /v1/messages 入口验收：Anthropic 协议客户端 → anthropic 上游（透传）
//! 与 → openai 上游（回向转换）。覆盖 x-api-key 鉴权、事件名透传、
//! 事件骨架合成、usage/cache 计费、错误壳。
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

// ---- mock 上游：anthropic 与 openai 两种 ----

fn sse_named(events: &[(&str, Value)]) -> String {
    let mut out = String::new();
    for (event, data) in events {
        let _ = write!(out, "event: {event}\ndata: {data}\n\n");
    }
    out
}

async fn mock_anthropic(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert!(headers.get("x-api-key").is_some());
    let req: Value = serde_json::from_slice(&body).unwrap();
    // 透传语义：客户端原始字段应原样到达（仅 model 重写）
    assert_eq!(req["system"], "keep me");
    if req["stream"].as_bool().unwrap_or(false) {
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            sse_named(&[
                (
                    "message_start",
                    json!({"type":"message_start","message":{"id":"msg_a","model":"claude-real",
                        "usage":{"input_tokens":100,"cache_read_input_tokens":800,"output_tokens":1}}}),
                ),
                (
                    "content_block_start",
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                ),
                (
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":0,
                        "delta":{"type":"text_delta","text":"native"}}),
                ),
                ("content_block_stop", json!({"type":"content_block_stop","index":0})),
                (
                    "message_delta",
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                        "usage":{"output_tokens":50}}),
                ),
                ("message_stop", json!({"type":"message_stop"})),
            ]),
        )
            .into_response()
    } else {
        axum::Json(json!({
            "id":"msg_a","type":"message","role":"assistant","model":"claude-real",
            "content":[{"type":"text","text":"native"}],
            "stop_reason":"end_turn",
            "usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":800}
        }))
        .into_response()
    }
}

async fn mock_openai(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    // 回向转换语义：system 应转为首条 system 消息
    assert_eq!(req["messages"][0]["role"], "system");
    assert_eq!(req["messages"][0]["content"], "keep me");
    if req["stream"].as_bool().unwrap_or(false) {
        assert_eq!(
            req["stream_options"],
            json!({"include_usage": true}),
            "转换必须强制 include_usage"
        );
        let chunks = [
            json!({"id":"cmpl-9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"id":"cmpl-9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"content":"from openai"}}]}),
            json!({"id":"cmpl-9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            json!({"id":"cmpl-9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[],"usage":{"prompt_tokens":900,"completion_tokens":50,
                    "prompt_tokens_details":{"cached_tokens":800}}}),
        ];
        let mut body = String::new();
        for c in &chunks {
            let _ = write!(body, "data: {c}\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
            .into_response()
    } else {
        axum::Json(json!({
            "id":"cmpl-9","object":"chat.completion","model":"gpt-real",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"from openai"}}],
            "usage":{"prompt_tokens":900,"completion_tokens":50,
                "prompt_tokens_details":{"cached_tokens":800}}
        }))
        .into_response()
    }
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/anthropic/v1/messages", post(mock_anthropic))
        .route("/openai/v1/chat/completions", post(mock_openai));
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

/// provider: "anthropic" | "openai"
async fn setup(provider: &str) -> TestEnv {
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
        &format!("{provider}-{suffix}"),
        provider,
        &format!("http://{mock}/{provider}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
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

/// Anthropic 协议请求（x-api-key 鉴权，非 Bearer）。
async fn post_messages(env: &TestEnv, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .header("x-api-key", &env.token)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": env.model,
            "stream": stream,
            "max_tokens": 256,
            "system": "keep me",
            "messages": [{"role": "user", "content": "hi there"}]
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

fn parse_named_events(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut event: Option<String> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("event: ") {
            event = Some(name.to_owned());
        } else if let Some(data) = line.strip_prefix("data: ")
            && let Some(name) = event.take()
            && let Ok(v) = serde_json::from_str::<Value>(data)
        {
            out.push((name, v));
        }
    }
    out
}

/// 透传：anthropic 入口 + anthropic 上游，事件名保留、计费一致。
#[tokio::test]
async fn messages_passthrough_anthropic_upstream() {
    let env = setup("anthropic").await;
    let resp = post_messages(&env, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let events = parse_named_events(&text);
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ],
        "原生事件名必须逐条透传：{text}"
    );
    assert!(!text.contains("[DONE]"), "anthropic 出口不得有 [DONE]");
    let delta = &events[2].1;
    assert_eq!(delta["delta"]["text"], "native");

    // (100×1 + 800×0.5 + 50×2) × 500 × 2 = 600_000
    let (status, amount, prompt, cached) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt, 900);
    assert_eq!(cached, 800);
    assert_eq!(amount, 600_000);
}

/// 回向转换：anthropic 入口 + openai 上游，合成 anthropic 事件骨架。
#[tokio::test]
async fn messages_converts_openai_upstream_stream() {
    let env = setup("openai").await;
    let resp = post_messages(&env, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let events = parse_named_events(&text);
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ],
        "必须合成完整 anthropic 事件骨架：{text}"
    );
    let content: String = events
        .iter()
        .filter(|(n, _)| n == "content_block_delta")
        .filter_map(|(_, d)| d["delta"]["text"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(content, "from openai");
    let (_, md) = events.iter().find(|(n, _)| n == "message_delta").unwrap();
    assert_eq!(md["usage"]["input_tokens"], 100, "anthropic 口径不含缓存");
    assert_eq!(md["usage"]["cache_read_input_tokens"], 800);
    assert_eq!(md["usage"]["output_tokens"], 50);

    let (status, amount, prompt, cached) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt, 900, "计费仍是 OpenAI 口径 prompt 含缓存");
    assert_eq!(cached, 800);
    assert_eq!(amount, 600_000);
}

/// 非流式回向转换 + 错误壳。
#[tokio::test]
async fn messages_json_and_error_envelope() {
    let env = setup("openai").await;
    let resp = post_messages(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["text"], "from openai");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["usage"]["input_tokens"], 100);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 800);

    let (_, amount, _, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(amount, 600_000);

    // 未知 key → anthropic 错误壳
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .header("x-api-key", "sk-bogus")
        .json(&json!({"model": env.model, "max_tokens": 10,
            "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_api_key");
}
