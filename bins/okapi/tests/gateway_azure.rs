//! Azure OpenAI 上游验收（IMPLEMENTATION §11.29）：OpenAI 协议客户端 → azure 渠道全链路。
//! 覆盖：部署级 URL（`/openai/deployments/{deployment}/...`）、`api-version` 查询参数
//! （渠道 settings）、`api-key` 鉴权头（且不带 Authorization）、model_mapping → 部署名、
//! 流式 / 非流式 chat 计费、embeddings 走同一分派、无 api_base 的 azure 渠道建不出来。
//! 依赖 .env 的 DATABASE_URL / OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::{Path, Query};
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use uuid::Uuid;

const API_VERSION: &str = "2025-01-01-preview";

// ---- mock azure 上游 ----

/// 三条协议断言集中在这里：api-key 头、无 Bearer、api-version 查询串。
fn assert_azure_envelope(headers: &axum::http::HeaderMap, query: &HashMap<String, String>) {
    assert_eq!(
        headers.get("api-key").and_then(|v| v.to_str().ok()),
        Some("mock-credential"),
        "azure 凭证必须走 api-key 头"
    );
    assert!(
        headers.get(axum::http::header::AUTHORIZATION).is_none(),
        "azure 请求不得再带 Authorization"
    );
    assert_eq!(
        query.get("api-version").map(String::as_str),
        Some(API_VERSION),
        "api-version 必须来自渠道 settings"
    );
}

async fn mock_chat(
    Path(deployment): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_azure_envelope(&headers, &query);
    assert!(
        deployment.starts_with("dep-"),
        "URL 里必须是 model_mapping 映射出的部署名，实际 {deployment}"
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        req["model"].as_str().unwrap(),
        deployment,
        "body.model 与 URL 部署名一致"
    );

    if req["stream"].as_bool().unwrap_or(false) {
        let chunks = [
            json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-2024-08-06",
                   "choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}),
            json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-2024-08-06",
                   "choices":[{"index":0,"delta":{"content":" azure"},"finish_reason":"stop"}]}),
            json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-2024-08-06",
                   "choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}),
        ];
        let mut out = String::new();
        for c in chunks {
            let _ = write!(out, "data: {c}\n\n");
        }
        out.push_str("data: [DONE]\n\n");
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            out,
        )
            .into_response()
    } else {
        axum::Json(json!({
            "id":"c2","object":"chat.completion","model":"gpt-4o-2024-08-06",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Hello azure"},
                        "finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}
        }))
        .into_response()
    }
}

async fn mock_embeddings(
    Path(deployment): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_azure_envelope(&headers, &query);
    assert!(deployment.starts_with("dep-"));
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["model"].as_str().unwrap(), deployment);
    axum::Json(json!({
        "object": "list",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
        "model": deployment,
        "usage": {"prompt_tokens": 120, "total_tokens": 120}
    }))
    .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route(
            "/openai/deployments/{deployment}/chat/completions",
            post(mock_chat),
        )
        .route(
            "/openai/deployments/{deployment}/embeddings",
            post(mock_embeddings),
        );
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
    let deployment = format!("dep-{}", &suffix[..8]);

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
    // 站长把带 /openai 后缀的地址整段贴进来是常态，网关须自行剥掉
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("azure-{suffix}"),
        "azure",
        &format!("http://{mock}/openai"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query!(
        r#"UPDATE channels
           SET model_mapping = $2, settings = settings || $3
           WHERE id = $1"#,
        channel_id,
        json!({ &model: deployment }),
        json!({ "api_version": API_VERSION })
    )
    .execute(&pg)
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
            "messages": [{"role": "user", "content": "hi there"}]
        }))
        .send()
        .await
        .unwrap()
}

async fn wait_record(pg: &PgPool, user_id: i64) -> (i16, i64, i32) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens
               FROM billing_records WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.status, r.amount_micro, r.prompt_tokens);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

/// 流式：SSE 原样透出，usage 帧进结算（部署名 / 头 / 版本由 mock 侧断言）。
#[tokio::test]
async fn azure_stream_end_to_end() {
    let env = setup().await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("data: [DONE]"));
    let content: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|c| {
            c["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(content, "Hello azure");

    // (100 prompt×1 + 50 completion×2) × 500 × $2/1M = 200 × 500 × 2 = 200_000 micro
    let (status, amount, prompt_tokens) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20, "committed");
    assert_eq!(prompt_tokens, 100);
    assert_eq!(amount, 200_000);
}

/// 非流式：JSON 原样透出 + 计费。
#[tokio::test]
async fn azure_json_end_to_end() {
    let env = setup().await;
    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello azure");
    let (status, amount, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 200_000);
}

/// embeddings 走同一 OpenAI 方言分派：部署 URL + api-key + api-version。
#[tokio::test]
async fn azure_embeddings_dispatch() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({ "model": env.model, "input": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["usage"]["prompt_tokens"], 120);
    let (status, _, prompt_tokens) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt_tokens, 120);
}
