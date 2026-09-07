//! Playground 同源中继 + 站点预设端点验收（IMPLEMENTATION §11.39）。
//!
//! 中继在 console 进程内直接调用数据面处理器：鉴权 / 限流 / 计费与真实 SDK 调用一致。
//! 覆盖：强制流式（body 里 stream:false 也回 SSE）、SSE 逐块透出、记账落在同一把 key、
//! 无 key 401、超 1MB 413；`GET /api/playground/presets` 白名单收口。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

/// mock OpenAI 上游：只认流式（中继会强制 stream:true），逐块回内容 + usage + [DONE]。
async fn mock_stream(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["stream"], true, "中继必须把请求改成流式");
    let chunks = [
        json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-mock",
               "choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}),
        json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-mock",
               "choices":[{"index":0,"delta":{"content":" playground"},"finish_reason":"stop"}]}),
        json!({"id":"c1","object":"chat.completion.chunk","model":"gpt-4o-mock",
               "choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}}),
    ];
    let mut out = String::new();
    for c in chunks {
        out.push_str("data: ");
        out.push_str(&c.to_string());
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        out,
    )
        .into_response()
}

struct TestEnv {
    pg: PgPool,
    state: gateway::state::AppState,
    addr: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("pg-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("pg-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-pg-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &hash, "sk-okapi-pg")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock_app = axum::Router::new().route("/v1/chat/completions", post(mock_stream));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_app).await.unwrap();
    });
    okapi_store::provision::create_channel(
        &pg,
        &format!("pg-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
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
    // 站点预设经进程缓存注入（不写共享库，不干扰并行用例）
    state
        .settings_cache
        .insert(
            "playground_presets".to_owned(),
            Arc::new(Some(json!([
                {"name": " Reviewer ", "model": "gpt-4o", "system": "be terse",
                 "temperature": 9, "max_tokens": 0, "top_p": -1, "secret": "x"},
                {"name": "", "model": "gpt-4o"},
                {"model": "no-name"},
                {"name": "Plain", "model": "claude-3", "max_tokens": 512}
            ]))),
        )
        .await;

    let app = console::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        state,
        addr,
        token,
        user_id,
        model,
    }
}

async fn wait_committed(pg: &PgPool, user_id: i64) -> (i16, i64, i32) {
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

/// 中继：body 里 stream:false 也回 SSE，逐块透出内容与 [DONE]，记账落在同一把 key。
#[tokio::test]
async fn relay_forces_stream_and_bills() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "stream": false,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
        "非流式请求应被强制为流式"
    );
    let text = resp.text().await.unwrap();
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
    assert_eq!(content, "Hello playground");
    assert!(text.contains("data: [DONE]"));

    // 倍率全 1 → 基价 $0.002/1K = 2 micro/token：(100 prompt + 20 completion) × 2 = 240 micro
    let (status, amount, prompt) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(status, 20, "committed");
    assert_eq!(prompt, 100);
    assert_eq!(amount, 240);
}

/// 无凭证：与真实数据面一致 401（中继不绕过鉴权）。
#[tokio::test]
async fn relay_requires_key() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

/// 请求体超 1MB：413（在改写流式之前就拦）。
#[tokio::test]
async fn relay_rejects_oversized_body() {
    let env = setup().await;
    let big = "x".repeat(1024 * 1024 + 16);
    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/me/playground/chat", env.addr))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "messages": [{"role": "user", "content": big}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["param"], "body_too_large");
}

/// 站点预设端点：公开只读、白名单收口（缺 name/model 丢弃、越界夹取、多余字段不出）。
#[tokio::test]
async fn presets_endpoint_whitelists() {
    let env = setup().await;
    // 公开端点：不带任何凭证也能读
    let resp = reqwest::Client::new()
        .get(format!("http://{}/api/playground/presets", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2, "缺 name / model 的两条被丢弃：{body}");
    assert_eq!(data[0]["name"], "Reviewer", "首尾空白去掉");
    assert_eq!(data[0]["temperature"], 2.0, "温度夹到 2");
    assert!(data[0]["max_tokens"].is_null(), "0 视为未设置");
    assert_eq!(data[0]["top_p"], 0.0, "top_p 夹到 0");
    assert!(data[0].get("secret").is_none(), "多余字段不出");
    assert_eq!(data[1]["name"], "Plain");
    assert_eq!(data[1]["max_tokens"], 512);
    // state 已被 setup 用掉一次；这里只是防止未使用告警
    let _ = &env.state;
}
