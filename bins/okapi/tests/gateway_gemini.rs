//! M3 gemini 上游验收：OpenAI 协议客户端 → gemini 渠道全链路。
//! 覆盖：URL 路径模型名 + x-goog-api-key、SSE chunk 回转 OpenAI、usage 计费、非流式。
//! 依赖 .env 的 DATABASE_URL / OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_generate(
    Path(model_and_action): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_eq!(
        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
        Some("mock-credential"),
        "gemini 凭证必须走 x-goog-api-key"
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["systemInstruction"]["parts"][0]["text"], "sys prompt");
    assert_eq!(req["contents"][0]["role"], "user");
    assert!(req.get("model").is_none(), "gemini 模型名走 URL，body 不带");

    if model_and_action.ends_with(":streamGenerateContent") {
        let chunks = [
            json!({"candidates": [{"content": {"parts": [{"text": "Hello"}]}}]}),
            json!({"candidates": [{"content": {"parts": [{"text": " gemini"}]},
                "finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 900, "candidatesTokenCount": 40,
                    "cachedContentTokenCount": 800, "thoughtsTokenCount": 10}}),
        ];
        let mut out = String::new();
        for c in &chunks {
            let _ = write!(out, "data: {c}\r\n\r\n");
        }
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            out,
        )
            .into_response()
    } else {
        axum::Json(json!({
            "responseId": "r-1",
            "candidates": [{"content": {"role": "model",
                "parts": [{"text": "Hello gemini"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 900, "candidatesTokenCount": 40,
                "cachedContentTokenCount": 800, "thoughtsTokenCount": 10}
        }))
        .into_response()
    }
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new().route("/v1beta/models/{model_and_action}", post(mock_generate));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

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
        &format!("gemini-{suffix}"),
        "gemini",
        &format!("http://{mock}/v1beta"),
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

/// 流式：gemini chunk 回转 OpenAI；usage（含 thoughts）计费。
/// (100 非缓存×1 + 800 缓存×0.5 + 50 补全×2) × 500 × 2 = 600_000
#[tokio::test]
async fn gemini_stream_end_to_end() {
    let env = setup().await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("data: [DONE]"), "OpenAI 出口必须 [DONE] 收尾");
    let chunks: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| serde_json::from_str(d).ok())
        .collect();
    let content: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(content, "Hello gemini");
    let usage = chunks
        .iter()
        .find_map(|c| c.get("usage").filter(|u| !u.is_null()))
        .expect("必须有 usage chunk");
    assert_eq!(usage["prompt_tokens"], 900);
    assert_eq!(
        usage["completion_tokens"], 50,
        "candidates 40 + thoughts 10"
    );

    let (status, amount, prompt, cached) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt, 900);
    assert_eq!(cached, 800);
    assert_eq!(amount, 600_000);
}

/// 非流式：gemini JSON 回转 OpenAI chat.completion。
#[tokio::test]
async fn gemini_json_end_to_end() {
    let env = setup().await;
    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello gemini");
    assert_eq!(body["usage"]["prompt_tokens"], 900);

    let (status, amount, _, cached) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(cached, 800);
    assert_eq!(amount, 600_000);
}
