//! Gemini 原生入口验收：`POST /v1beta/models/{model}:generateContent|streamGenerateContent`。
//!
//! - 同方言直转（gemini 渠道）：请求体原样到上游（模型名走 URL、mapping 重写）、SSE chunk 原样
//!   透出、usage 取 usageMetadata（promptTokenCount 含缓存、thoughts 归 completion）；
//! - 转换（openai 渠道）：gemini→chat 请求 + chat→gemini 响应/流，出口无 `[DONE]`；
//! - 两跳（anthropic 渠道）：gemini→chat→anthropic，systemInstruction 抽到顶层 system；
//! - 鉴权三态：Bearer / `x-goog-api-key` / `?key=`；错误壳是 google.rpc.Status；
//! - `GET /v1beta/models` 是 Gemini `models.list` 形状。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

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

// ---- mock 上游 ----

const USAGE_META: &str = r#"{"promptTokenCount": 900, "candidatesTokenCount": 40,
    "cachedContentTokenCount": 800, "thoughtsTokenCount": 10}"#;

/// gemini 上游：断言直转**原样**（systemInstruction / generationConfig / tools 一个都不能改），
/// 模型名在 URL 且已按 model_mapping 重写。
async fn mock_gemini(
    Path(model_and_action): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_eq!(
        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
        Some("mock-credential")
    );
    let (model, action) = model_and_action.rsplit_once(':').unwrap();
    assert_eq!(model, "gemini-upstream", "model_mapping 必须重写到 URL");
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["systemInstruction"]["parts"][0]["text"], "sys prompt");
    assert_eq!(req["contents"][0]["role"], "user");
    assert_eq!(req["contents"][0]["parts"][0]["text"], "hi there");
    assert_eq!(req["generationConfig"]["maxOutputTokens"], 256);
    assert_eq!(
        req["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true}),
        "gemini 私有字段直转必须保住"
    );
    assert_eq!(req["tools"][0]["googleSearch"], json!({}), "内置工具原样");
    assert!(req.get("model").is_none(), "gemini 模型名走 URL，body 不带");
    assert!(req.get("messages").is_none(), "直转不能夹带 chat 方言");
    let usage: Value = serde_json::from_str(USAGE_META).unwrap();
    if action == "streamGenerateContent" {
        let chunks = [
            json!({"candidates": [{"content": {"parts": [{"text": "thinking…", "thought": true}], "role": "model"}, "index": 0}],
                "modelVersion": "gemini-upstream-001"}),
            json!({"candidates": [{"content": {"parts": [{"text": "Hello"}], "role": "model"}, "index": 0}]}),
            json!({"candidates": [{"content": {"parts": [{"text": " gemini"}], "role": "model"},
                "finishReason": "STOP", "index": 0}],
                "usageMetadata": usage, "modelVersion": "gemini-upstream-001"}),
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
            "modelVersion": "gemini-upstream-001",
            "candidates": [{"content": {"role": "model",
                "parts": [{"text": "Hello gemini"}]}, "finishReason": "STOP", "index": 0}],
            "usageMetadata": usage
        }))
        .into_response()
    }
}

/// openai 上游：断言 gemini→chat 的映射语义。
async fn mock_chat(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["messages"][0]["role"], "system");
    assert_eq!(req["messages"][0]["content"], "sys prompt");
    assert_eq!(req["messages"][1]["role"], "user");
    assert_eq!(req["messages"][1]["content"], "hi there");
    assert_eq!(req["max_tokens"], 256, "maxOutputTokens 必须映射");
    assert!(
        req.get("contents").is_none() && req.get("generationConfig").is_none(),
        "chat 上游不能收到 gemini 方言：{req}"
    );
    if req["stream"].as_bool().unwrap_or(false) {
        assert_eq!(req["stream_options"], json!({"include_usage": true}));
        let chunks = [
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"content":"Hello "}}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"content":"chat"}}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real","choices":[],
                "usage":{"prompt_tokens":100,"completion_tokens":20,
                    "prompt_tokens_details":{"cached_tokens":0}}}),
        ];
        let mut out = String::new();
        for c in &chunks {
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
            "id":"c9","object":"chat.completion","model":"gpt-real",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"Hello chat"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20}
        }))
        .into_response()
    }
}

/// anthropic 上游：两跳 gemini→chat→anthropic，system 抽到顶层。
async fn mock_anthropic(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["system"], "sys prompt");
    assert_eq!(req["messages"][0]["role"], "user");
    assert!(req["max_tokens"].as_u64().unwrap() >= 256);
    axum::Json(json!({
        "id":"msg_1","type":"message","role":"assistant","model":"claude-real",
        "content":[{"type":"text","text":"Hello claude"}],
        "stop_reason":"end_turn",
        "usage":{"input_tokens":100,"output_tokens":20}
    }))
    .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/g/v1beta/models/{model_and_action}", post(mock_gemini))
        .route("/oai/v1/chat/completions", post(mock_chat))
        .route("/ant/v1/messages", post(mock_anthropic));
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

/// ratio 1/1/1：$2/M 一价，便于口算。gemini 渠道配 model_mapping → gemini-upstream。
async fn setup(provider: &str, path: &str) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

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
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        provider,
        &format!("http://{mock}{path}"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    if provider == "gemini" {
        sqlx::query("UPDATE channels SET model_mapping = $2 WHERE id = $1")
            .bind(channel_id)
            .bind(json!({ model.as_str(): "gemini-upstream" }))
            .execute(&pg)
            .await
            .unwrap();
    }

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

fn gemini_body() -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": "sys prompt"}]},
        "contents": [{"role": "user", "parts": [{"text": "hi there"}]}],
        "generationConfig": {"maxOutputTokens": 256, "thinkingConfig": {"includeThoughts": true}},
        "tools": [{"googleSearch": {}}]
    })
}

enum Auth<'a> {
    Bearer,
    GoogHeader,
    QueryKey,
    Wrong(&'a str),
}

async fn post_gemini(env: &TestEnv, stream: bool, auth: Auth<'_>) -> reqwest::Response {
    let action = if stream {
        "streamGenerateContent"
    } else {
        "generateContent"
    };
    let mut url = format!(
        "http://{}/v1beta/models/{}:{action}?alt={}",
        env.gateway,
        env.model,
        if stream { "sse" } else { "json" }
    );
    if matches!(auth, Auth::QueryKey) {
        let _ = write!(url, "&key={}", env.token);
    }
    let mut req = reqwest::Client::new().post(&url);
    req = match auth {
        Auth::Bearer => req.bearer_auth(&env.token),
        Auth::GoogHeader => req.header("x-goog-api-key", &env.token),
        Auth::QueryKey => req,
        Auth::Wrong(bad) => req.header("x-goog-api-key", bad),
    };
    req.json(&gemini_body()).send().await.unwrap()
}

async fn wait_record(pg: &PgPool, user_id: i64) -> (i16, i64, i32, i32, i32) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens, completion_tokens, cached_tokens
               FROM billing_records WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (
                r.status,
                r.amount_micro,
                r.prompt_tokens,
                r.completion_tokens,
                r.cached_tokens,
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

fn sse_datas(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str(d).ok())
        .collect()
}

fn texts(chunks: &[Value]) -> String {
    chunks
        .iter()
        .filter_map(|c| c["candidates"][0]["content"]["parts"].as_array())
        .flatten()
        .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|p| p["text"].as_str())
        .collect()
}

// ---- 直转：gemini 入口 → gemini 渠道 ----

/// 流式：chunk 原样透出（thought part、modelVersion 都保留，无 [DONE]）；
/// usage：prompt 900（含 cached 800）、completion 40+10；ratio 1/1/1 → (900+50)×$2/M = 1900。
#[tokio::test]
async fn native_stream_passthrough_and_billing() {
    let env = setup("gemini", "/g/v1beta").await;
    let resp = post_gemini(&env, true, Auth::GoogHeader).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"))
    );
    let text = resp.text().await.unwrap();
    assert!(!text.contains("[DONE]"), "Gemini 出口无 [DONE]：{text}");
    let chunks = sse_datas(&text);
    assert_eq!(chunks.len(), 3, "上游 3 个 chunk 必须原样透出：{text}");
    assert_eq!(
        chunks[0]["candidates"][0]["content"]["parts"][0]["thought"],
        true
    );
    assert_eq!(texts(&chunks), "Hello gemini");
    assert_eq!(chunks[2]["candidates"][0]["finishReason"], "STOP");
    assert_eq!(chunks[2]["usageMetadata"]["promptTokenCount"], 900);
    assert_eq!(chunks[2]["modelVersion"], "gemini-upstream-001");

    let (status, amount, prompt, completion, cached) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!((prompt, completion, cached), (900, 50, 800));
    assert_eq!(amount, 1_900);
}

/// 非流式：JSON 原样透出；`?key=` 鉴权。
#[tokio::test]
async fn native_json_passthrough_with_query_key_auth() {
    let env = setup("gemini", "/g/v1beta").await;
    let resp = post_gemini(&env, false, Auth::QueryKey).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["responseId"], "r-1", "直转不得改写响应对象");
    assert_eq!(
        body["candidates"][0]["content"]["parts"][0]["text"],
        "Hello gemini"
    );
    assert_eq!(body["usageMetadata"]["cachedContentTokenCount"], 800);

    let (status, amount, _, _, cached) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(cached, 800);
    assert_eq!(amount, 1_900);
}

// ---- 转换：gemini 入口 → openai 渠道 ----

/// 流式：chat chunk → gemini chunk；文本增量逐 chunk、最后一个 chunk 带 finishReason + usageMetadata；
/// 无 [DONE]。ratio 1/1/1 → (100+20)×$2/M = 240。
#[tokio::test]
async fn openai_channel_stream_converted() {
    let env = setup("openai", "/oai/v1").await;
    let resp = post_gemini(&env, true, Auth::Bearer).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let text = resp.text().await.unwrap();
    assert!(!text.contains("[DONE]"), "Gemini 出口无 [DONE]：{text}");
    let chunks = sse_datas(&text);
    assert!(chunks.len() >= 3, "{text}");
    assert_eq!(texts(&chunks), "Hello chat");
    for c in &chunks {
        assert_eq!(c["candidates"][0]["content"]["role"], "model");
        assert!(c.get("choices").is_none(), "出口不能夹带 chat 方言：{c}");
    }
    let last = chunks.last().unwrap();
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    assert_eq!(last["usageMetadata"]["promptTokenCount"], 100);
    assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 20);
    assert_eq!(last["usageMetadata"]["totalTokenCount"], 120);
    // 与其它转换器一致：报上游响应模型（bill_by_response_model 依赖此语义）
    assert_eq!(last["modelVersion"], "gpt-real");

    let (status, amount, prompt, completion, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!((prompt, completion), (100, 20));
    assert_eq!(amount, 240);
}

/// 非流式：chat.completion → GenerateContentResponse。
#[tokio::test]
async fn openai_channel_json_converted() {
    let env = setup("openai", "/oai/v1").await;
    let resp = post_gemini(&env, false, Auth::Bearer).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["candidates"][0]["content"]["role"], "model");
    assert_eq!(
        body["candidates"][0]["content"]["parts"][0]["text"],
        "Hello chat"
    );
    assert_eq!(body["candidates"][0]["finishReason"], "STOP");
    assert_eq!(body["usageMetadata"]["promptTokenCount"], 100);
    assert_eq!(body["usageMetadata"]["candidatesTokenCount"], 20);
    assert!(body.get("choices").is_none() && body.get("object").is_none());

    let (status, amount, _, _, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
}

// ---- 两跳：gemini 入口 → anthropic 渠道 ----

#[tokio::test]
async fn anthropic_channel_two_hops() {
    let env = setup("anthropic", "/ant/v1").await;
    let resp = post_gemini(&env, false, Auth::GoogHeader).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["candidates"][0]["content"]["parts"][0]["text"],
        "Hello claude"
    );
    assert_eq!(body["candidates"][0]["finishReason"], "STOP");
    assert_eq!(body["usageMetadata"]["promptTokenCount"], 100);

    let (status, amount, _, _, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
}

// ---- 错误壳 / 模型列表 ----

#[tokio::test]
async fn errors_are_google_rpc_status_shaped() {
    let env = setup("gemini", "/g/v1beta").await;

    // 鉴权失败 → 401 UNAUTHENTICATED
    let resp = post_gemini(&env, false, Auth::Wrong("sk-nope")).await;
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], 401);
    assert_eq!(body["error"]["status"], "UNAUTHENTICATED");
    assert!(body["error"]["message"].is_string());
    assert!(body.get("type").is_none(), "不能是 anthropic 壳");

    // 未知 action → 400 INVALID_ARGUMENT
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/v1beta/models/{}:countTokens",
            env.gateway, env.model
        ))
        .bearer_auth(&env.token)
        .json(&gemini_body())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");

    // 未知模型 → 404 NOT_FOUND
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/v1beta/models/no-such-model:generateContent",
            env.gateway
        ))
        .bearer_auth(&env.token)
        .json(&gemini_body())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], 404);
    assert_eq!(body["error"]["status"], "NOT_FOUND");
}

#[tokio::test]
async fn models_list_is_gemini_shaped() {
    let env = setup("gemini", "/g/v1beta").await;
    let resp = reqwest::Client::new()
        .get(format!("http://{}/v1beta/models", env.gateway))
        .header("x-goog-api-key", &env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let models = body["models"].as_array().unwrap();
    let mine = models
        .iter()
        .find(|m| m["name"] == format!("models/{}", env.model))
        .expect("必须列出本用例模型");
    assert_eq!(
        mine["supportedGenerationMethods"],
        json!(["generateContent", "streamGenerateContent"])
    );
}
