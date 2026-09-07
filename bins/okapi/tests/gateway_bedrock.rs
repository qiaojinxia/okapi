//! Amazon Bedrock 上游验收（IMPLEMENTATION §11.35）：OpenAI / Anthropic 协议客户端 → bedrock 渠道全链路。
//! mock 上游用同一 secret 重算 SigV4 签名做断言；流式响应用 event-stream 二进制帧；
//! 另有 Bedrock API key（Bearer）形态。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::OriginalUri;
use axum::response::IntoResponse;
use axum::routing::post;
use base64::Engine as _;
use okapi::gateway;
use okapi_domain::Money;
use okapi_providers::aws_eventstream::encode_frame;
use okapi_providers::aws_sigv4::{AwsCredentials, SignParams, payload_hash, sign};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const API_KEY: &str = "ABSKQmVkcm9ja0FQSUtleS1hYmMxMjM";
const MODEL_ID: &str = "us.anthropic.claude-sonnet-4-5-20250929-v1:0";

// ---- mock bedrock-runtime ----

/// 鉴权断言：SigV4 形态用同一 secret 重算签名；API key 形态必须是 Bearer 且无 SigV4 头。
fn assert_auth(
    headers: &axum::http::HeaderMap,
    uri: &axum::http::Uri,
    body: &[u8],
    mock: SocketAddr,
) {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .expect("必须带 Authorization");
    if let Some(rest) = auth.strip_prefix("AWS4-HMAC-SHA256 ") {
        let field = |name: &str| {
            rest.split(", ")
                .find_map(|kv| kv.strip_prefix(name))
                .unwrap_or_default()
                .to_owned()
        };
        let credential = field("Credential=");
        let signed = field("SignedHeaders=");
        let signature = field("Signature=");
        let mut scope = credential.split('/');
        assert_eq!(scope.next(), Some(ACCESS_KEY));
        let date = scope.next().unwrap();
        assert_eq!(
            scope.next(),
            Some("us-east-1"),
            "region 来自 settings.aws_region"
        );
        assert_eq!(scope.next(), Some("bedrock"));
        assert_eq!(
            signed, "accept;content-type;host;x-amz-content-sha256;x-amz-date",
            "签名头集合"
        );
        let amz_date = headers
            .get("x-amz-date")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(amz_date.starts_with(date));
        let hash = payload_hash(body);
        assert_eq!(
            headers
                .get("x-amz-content-sha256")
                .unwrap()
                .to_str()
                .unwrap(),
            hash
        );
        // 用请求里的头值 + 同一 secret 重算：与网关算出的签名必须逐字相等
        let creds = AwsCredentials {
            access_key_id: ACCESS_KEY.to_owned(),
            secret_access_key: SECRET.to_owned(),
            session_token: None,
        };
        let url = reqwest::Url::parse(&format!("http://{mock}{}", uri.path())).unwrap();
        let ts = chrono::NaiveDateTime::parse_from_str(&amz_date, "%Y%m%dT%H%M%SZ")
            .unwrap()
            .and_utc();
        let recomputed = sign(
            &creds,
            &SignParams {
                method: "POST",
                url: &url,
                region: "us-east-1",
                service: "bedrock",
                headers: &[
                    ("content-type", "application/json"),
                    ("accept", headers.get("accept").unwrap().to_str().unwrap()),
                    ("x-amz-content-sha256", &hash),
                ],
                payload_hash: &hash,
                timestamp: ts,
            },
        );
        let expected = recomputed
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.rsplit("Signature=").next().unwrap().to_owned())
            .unwrap();
        assert_eq!(signature, expected, "SigV4 签名不一致");
    } else {
        assert_eq!(auth, format!("Bearer {API_KEY}"), "API key 形态走 Bearer");
        assert!(headers.get("x-amz-date").is_none(), "Bearer 形态不签 SigV4");
    }
}

fn assert_invoke_body(body: &[u8]) -> Value {
    let req: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(req["anthropic_version"], "bedrock-2023-05-31");
    assert!(
        req.get("model").is_none(),
        "模型在 URL 上，body 不得带 model"
    );
    assert!(
        req.get("stream").is_none(),
        "流式与否在 URL 上，body 不得带 stream"
    );
    assert!(req["max_tokens"].as_u64().unwrap() > 0);
    req
}

fn anthropic_json() -> Value {
    json!({"id":"msg_br","type":"message","role":"assistant","model":MODEL_ID,
           "content":[{"type":"text","text":"Hello bedrock"}],
           "stop_reason":"end_turn","stop_sequence":null,
           "usage":{"input_tokens":100,"output_tokens":50}})
}

fn eventstream_body() -> Vec<u8> {
    let events = [
        json!({"type":"message_start","message":{"id":"msg_br","model":MODEL_ID,
            "usage":{"input_tokens":100,"output_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" bedrock"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}),
        json!({"type":"message_stop"}),
    ];
    let mut out = Vec::new();
    for ev in events {
        let b64 = base64::engine::general_purpose::STANDARD.encode(ev.to_string());
        out.extend(encode_frame(
            &[
                (":message-type", "event"),
                (":event-type", "chunk"),
                (":content-type", "application/json"),
            ],
            format!(r#"{{"bytes":"{b64}"}}"#).as_bytes(),
        ));
    }
    out
}

#[derive(Clone)]
struct MockState {
    addr: SocketAddr,
}

async fn mock_invoke(
    axum::extract::State(st): axum::extract::State<MockState>,
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert!(
        uri.path().ends_with("-v1%3A0/invoke"),
        "模型 ID 里的冒号必须编成 %3A，实际 {}",
        uri.path()
    );
    assert_auth(&headers, &uri, &body, st.addr);
    assert_invoke_body(&body);
    (
        [("x-amzn-requestid", "req-bedrock-1")],
        axum::Json(anthropic_json()),
    )
        .into_response()
}

async fn mock_invoke_stream(
    axum::extract::State(st): axum::extract::State<MockState>,
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert!(uri.path().ends_with("-v1%3A0/invoke-with-response-stream"));
    assert_auth(&headers, &uri, &body, st.addr);
    assert_invoke_body(&body);
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/vnd.amazon.eventstream",
        )],
        eventstream_body(),
    )
        .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/model/{model_id}/invoke", post(mock_invoke))
        .route(
            "/model/{model_id}/invoke-with-response-stream",
            post(mock_invoke_stream),
        )
        .with_state(MockState { addr });
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

async fn setup(credential: &str) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-br-{}", &suffix[..10]);

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
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("bedrock-{suffix}"),
        "bedrock",
        &format!("http://{mock}"),
        credential,
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    // 127.0.0.1 解析不出区域：走 settings.aws_region（VPC 端点同一路径）
    sqlx::query!(
        r#"UPDATE channels SET model_mapping = $2, settings = settings || $3 WHERE id = $1"#,
        channel_id,
        json!({ &model: MODEL_ID }),
        json!({ "aws_region": "us-east-1" })
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
            "model": env.model, "stream": stream, "max_tokens": 256,
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

fn sigv4_credential() -> String {
    format!("{ACCESS_KEY}:{SECRET}")
}

/// SigV4 + event-stream 流式：帧还原成 Anthropic 事件 → OpenAI SSE，usage 进结算。
#[tokio::test]
async fn bedrock_sigv4_stream_end_to_end() {
    let env = setup(&sigv4_credential()).await;
    let resp = post_chat(&env, true).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
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
    assert_eq!(content, "Hello bedrock");
    // (100 prompt×1 + 50 completion×2) × 500 × $2/1M = 200_000 micro（与 azure 用例同价表）
    let (status, amount, prompt_tokens) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt_tokens, 100);
    assert_eq!(amount, 200_000);
}

/// SigV4 非流式：InvokeModel JSON → OpenAI JSON + 计费。
#[tokio::test]
async fn bedrock_sigv4_json_end_to_end() {
    let env = setup(&sigv4_credential()).await;
    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello bedrock");
    let (status, amount, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 200_000);
}

/// Bedrock API key 形态：Bearer、不签 SigV4（mock 侧断言）。
#[tokio::test]
async fn bedrock_api_key_uses_bearer() {
    let env = setup(API_KEY).await;
    let resp = post_chat(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello bedrock");
}

/// Anthropic 入口（Claude Code 形态）+ bedrock 上游：Messages 透传，只换版本字段。
#[tokio::test]
async fn bedrock_anthropic_ingress_passthrough() {
    let env = setup(&sigv4_credential()).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .header("x-api-key", &env.token)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": env.model, "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi there"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["text"], "Hello bedrock");
    let (status, amount, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 200_000);
}

/// embeddings 不路由 bedrock 渠道：该模型只有 bedrock 候选 → 503 无可用渠道，而不是打出错误请求。
#[tokio::test]
async fn bedrock_not_routed_for_embeddings() {
    let env = setup(&sigv4_credential()).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({ "model": env.model, "input": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_available_channel");
}
