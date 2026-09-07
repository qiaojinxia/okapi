//! Google Vertex AI 上游验收（IMPLEMENTATION §11.35）：服务账号 OAuth + 两条 publisher 路径。
//! mock 同时扮演 token 端点与 aiplatform：断言 Bearer 来自换出的 token、token 只换一次（缓存）、
//! Gemini 走 publishers/google、Claude 走 publishers/anthropic 且版本字段换成 vertex 值。
//! 依赖 .env（scripts/dev-deps.sh up）。

use aws_lc_rs::encoding::AsDer as _;
use axum::Router;
use axum::extract::{OriginalUri, State};
use axum::response::IntoResponse;
use axum::routing::post;
use base64::Engine as _;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

const PROJECT_PATH: &str = "/v1/projects/p-okapi/locations/us-central1";
const GEMINI_ID: &str = "gemini-2.5-flash";
const CLAUDE_ID: &str = "claude-sonnet-4-5@20250929";

#[derive(Clone)]
struct Mock {
    token_calls: Arc<AtomicUsize>,
}

/// 现场生成一把 2048 位 RSA 私钥，拼成服务账号 JSON（token_uri 指向 mock）。
fn service_account_json(token_uri: &str) -> String {
    let key = aws_lc_rs::signature::RsaKeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
    let der = key.as_der().unwrap();
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
    for chunk in b64.as_bytes().chunks(64) {
        let _ = writeln!(pem, "{}", std::str::from_utf8(chunk).unwrap());
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    json!({
        "type": "service_account",
        "project_id": "p-okapi",
        "client_email": "okapi@p-okapi.iam.gserviceaccount.com",
        "private_key": pem,
        "token_uri": token_uri,
    })
    .to_string()
}

async fn mock_token(State(st): State<Mock>, body: String) -> axum::response::Response {
    let n = st.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(
        body.starts_with(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="
        ),
        "JWT-bearer 表单：{body}"
    );
    let assertion = body.split("assertion=").nth(1).unwrap();
    assert_eq!(assertion.split('.').count(), 3, "JWT 三段");
    axum::Json(json!({"access_token": format!("ya29.tok-{n}"), "expires_in": 3600, "token_type": "Bearer"}))
        .into_response()
}

fn assert_bearer(headers: &axum::http::HeaderMap) {
    assert_eq!(
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer ya29.tok-1"),
        "Bearer 必须是换出的 token，且整个用例只换一次"
    );
    assert!(headers.get("x-goog-api-key").is_none());
    assert!(headers.get("x-api-key").is_none());
}

async fn mock_google(
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_bearer(&headers);
    let tail = uri.path().rsplit('/').next().unwrap();
    let (model, action) = tail.split_once(':').unwrap();
    assert_eq!(model, GEMINI_ID, "URL 模型名 = 映射值");
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert!(req["contents"].is_array());
    if action == "streamGenerateContent" {
        assert_eq!(uri.query(), Some("alt=sse"));
        let chunks = [
            json!({"candidates": [{"content": {"parts": [{"text": "Hello"}]}}]}),
            json!({"candidates": [{"content": {"parts": [{"text": " vertex"}]}, "finishReason": "STOP"}],
                   "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 50, "totalTokenCount": 150}}),
        ];
        let mut out = String::new();
        for c in chunks {
            let _ = write!(out, "data: {c}\r\n\r\n");
        }
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            out,
        )
            .into_response()
    } else {
        assert_eq!(action, "generateContent");
        axum::Json(json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "Hello vertex"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 50, "totalTokenCount": 150}
        }))
        .into_response()
    }
}

async fn mock_anthropic(
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_bearer(&headers);
    let tail = uri.path().rsplit('/').next().unwrap();
    let (model, action) = tail.split_once(':').unwrap();
    assert_eq!(model, CLAUDE_ID);
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["anthropic_version"], "vertex-2023-10-16");
    assert!(req.get("model").is_none(), "模型在 URL 上");
    assert!(req["max_tokens"].as_u64().unwrap() > 0);
    if action == "streamRawPredict" {
        assert_eq!(req["stream"], true);
        let events = [
            (
                "message_start",
                json!({"type":"message_start","message":{"id":"msg_v","model":CLAUDE_ID,
                "usage":{"input_tokens":100,"output_tokens":1}}}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" vertex"}}),
            ),
            (
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ];
        let mut out = String::new();
        for (ev, data) in events {
            let _ = write!(out, "event: {ev}\ndata: {data}\n\n");
        }
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            out,
        )
            .into_response()
    } else {
        assert_eq!(action, "rawPredict");
        axum::Json(
            json!({"id":"msg_v","type":"message","role":"assistant","model":CLAUDE_ID,
            "content":[{"type":"text","text":"Hello vertex"}],"stop_reason":"end_turn",
            "usage":{"input_tokens":100,"output_tokens":50}}),
        )
        .into_response()
    }
}

async fn spawn_mock(mock: Mock) -> SocketAddr {
    let router = Router::new()
        .route("/token", post(mock_token))
        .route(
            &format!("{PROJECT_PATH}/publishers/google/models/{{model_action}}"),
            post(mock_google),
        )
        .route(
            &format!("{PROJECT_PATH}/publishers/anthropic/models/{{model_action}}"),
            post(mock_anthropic),
        )
        .with_state(mock);
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
    token_calls: Arc<AtomicUsize>,
}

async fn setup(upstream_model: &str) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-vx-{}", &suffix[..10]);

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

    let token_calls = Arc::new(AtomicUsize::new(0));
    let mock = spawn_mock(Mock {
        token_calls: Arc::clone(&token_calls),
    })
    .await;
    let credential = service_account_json(&format!("http://{mock}/token"));
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("vertex-{suffix}"),
        "vertex",
        &format!("http://{mock}{PROJECT_PATH}"),
        &credential,
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query!(
        r#"UPDATE channels SET model_mapping = $2 WHERE id = $1"#,
        channel_id,
        json!({ &model: upstream_model })
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
        token_calls,
    }
}

async fn post_chat(env: &TestEnv, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "stream": stream, "max_tokens": 256,
            "messages": [{"role": "user", "content": "hi there"}]
        }))
        .send()
        .await
        .unwrap()
}

fn sse_content(text: &str) -> String {
    text.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|c| {
            c["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_owned)
        })
        .collect()
}

async fn wait_records(pg: &PgPool, user_id: i64, n: usize) -> Vec<(i16, i64, i32)> {
    for _ in 0..50 {
        let rows = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens
               FROM billing_records WHERE user_id = $1 AND log_type = 2 ORDER BY id"#,
            user_id
        )
        .fetch_all(pg)
        .await
        .unwrap();
        if rows.len() >= n {
            return rows
                .into_iter()
                .map(|r| (r.status, r.amount_micro, r.prompt_tokens))
                .collect();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

/// Gemini on Vertex：流式 + 非流式各一次，token 端点只被打一次（缓存），两笔各计 200_000。
#[tokio::test]
async fn vertex_gemini_stream_and_json_share_one_token() {
    let env = setup(GEMINI_ID).await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let text = resp.text().await.unwrap();
    assert!(text.contains("data: [DONE]"));
    assert_eq!(sse_content(&text), "Hello vertex");

    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello vertex");

    let records = wait_records(&env.pg, env.user_id, 2).await;
    for (status, amount, prompt) in records {
        assert_eq!(status, 20);
        assert_eq!(prompt, 100);
        assert_eq!(amount, 200_000);
    }
    assert_eq!(
        env.token_calls.load(Ordering::SeqCst),
        1,
        "access token 应缓存复用"
    );
}

/// Claude on Vertex：rawPredict / streamRawPredict，版本字段为 vertex 值（mock 侧断言）。
#[tokio::test]
async fn vertex_claude_raw_predict_end_to_end() {
    let env = setup(CLAUDE_ID).await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(sse_content(&resp.text().await.unwrap()), "Hello vertex");

    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello vertex");

    // Anthropic 入口 + Vertex Claude：Messages 透传（Claude Code 经网关打 Vertex）
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .header("x-api-key", &env.token)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({"model": env.model, "max_tokens": 64,
                      "messages": [{"role": "user", "content": "hi there"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "Hello vertex");

    let records = wait_records(&env.pg, env.user_id, 3).await;
    assert!(
        records
            .iter()
            .all(|(status, amount, _)| *status == 20 && *amount == 200_000)
    );
    assert_eq!(env.token_calls.load(Ordering::SeqCst), 1);
}
