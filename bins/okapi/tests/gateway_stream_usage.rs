//! 回归：OpenAI 同方言流式必须向上游补 `stream_options.include_usage`。
//!
//! 缺这一帧上游不返 usage，结算退化为 `chars/4` 字符估算——中文场景实测
//! 405 汉字的 prompt 落账 108 tokens，漏收约七成。跨方言的三条路
//! （anthropic→openai / responses→chat / openai→anthropic）各自的转换器早已
//! 强制注入，本用例守住最常用的同方言路。
//! 依赖 .env 的 DATABASE_URL 与 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// 上游实际收到的请求体（断言注入结果）。
type Seen = Arc<Mutex<Vec<Value>>>;

const UP_PROMPT: u32 = 1000;
const UP_COMPLETION: u32 = 500;

/// mock 上游按真实 OpenAI 语义行事：**只有**被要求 include_usage 才发 usage 帧。
async fn spawn_mock(seen: Seen) -> SocketAddr {
    let handler = move |body: axum::body::Bytes| {
        let seen = Arc::clone(&seen);
        async move {
            let req: Value = serde_json::from_slice(&body).unwrap();
            let wants_usage = req
                .pointer("/stream_options/include_usage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let stream = req["stream"].as_bool().unwrap_or(false);
            seen.lock().unwrap().push(req);

            if !stream {
                return axum::Json(json!({
                    "id": "cmpl", "object": "chat.completion",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "你好"}}],
                    "usage": {"prompt_tokens": UP_PROMPT, "completion_tokens": UP_COMPLETION}
                }))
                .into_response();
            }

            let mut chunks = vec![json!({
                "id": "c", "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": {"content": "你好"}}]
            })];
            if wants_usage {
                chunks.push(json!({
                    "id": "c", "object": "chat.completion.chunk", "choices": [],
                    "usage": {"prompt_tokens": UP_PROMPT, "completion_tokens": UP_COMPLETION}
                }));
            }
            let mut out = String::new();
            for chunk in &chunks {
                let _ = write!(out, "data: {chunk}\n\n");
            }
            out.push_str("data: [DONE]\n\n");
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                out,
            )
                .into_response()
        }
    };
    let router = Router::new().route("/up/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Env {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    model: String,
    seen: Seen,
}

async fn setup() -> Env {
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
    let token = format!("sk-okapi-su-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-su")
        .await
        .unwrap();
    // 倍率全 1 → 每 token 2 micro（基准价 $2/1M）
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let mock = spawn_mock(Arc::clone(&seen)).await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        "openai",
        &format!("http://{mock}/up/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "su-node", None, None)
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

    Env {
        pg,
        gateway: addr,
        token,
        model,
        seen,
    }
}

async fn post_chat(env: &Env, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn wait_amount(pg: &PgPool, request_id: Uuid) -> (i64, i32, i32) {
    for _ in 0..50 {
        if let Some(r) = sqlx::query!(
            r#"SELECT amount_micro, prompt_tokens, completion_tokens
               FROM billing_records WHERE request_id = $1 AND status = 20"#,
            request_id
        )
        .fetch_optional(pg)
        .await
        .unwrap()
        {
            return (r.amount_micro, r.prompt_tokens, r.completion_tokens);
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

/// 主回归：客户端不声明 stream_options → 网关补 → 上游返 usage → 按真实 usage 结算。
#[tokio::test]
async fn stream_injects_include_usage_and_bills_by_real_usage() {
    let env = setup().await;
    let resp = post_chat(
        &env,
        json!({
            "model": env.model, "stream": true,
            "messages": [{"role": "user", "content": "中文提示词内容测试".repeat(45)}]
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let request_id = request_id_of(&resp);
    let _ = resp.text().await.unwrap();

    let upstream = env.seen.lock().unwrap().first().cloned().unwrap();
    assert_eq!(
        upstream.pointer("/stream_options/include_usage"),
        Some(&json!(true)),
        "同方言流式必须补 include_usage，否则结算落字符估算"
    );

    let (amount, prompt, completion) = wait_amount(&env.pg, request_id).await;
    assert_eq!(
        prompt,
        i32::try_from(UP_PROMPT).unwrap(),
        "按上游 usage 记账"
    );
    assert_eq!(completion, i32::try_from(UP_COMPLETION).unwrap());
    // 倍率全 1：(1000 + 500) × 2 micro/token
    assert_eq!(amount, 3000, "字符估算会显著低于此值");
}

/// 非流式不得注入：OpenAI 对 stream=false 携带 stream_options 判 400。
#[tokio::test]
async fn non_stream_does_not_get_stream_options() {
    let env = setup().await;
    let resp = post_chat(
        &env,
        json!({
            "model": env.model, "stream": false,
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let upstream = env.seen.lock().unwrap().first().cloned().unwrap();
    assert!(
        upstream.get("stream_options").is_none(),
        "非流式携带 stream_options 会被上游判 400，实收 {upstream}"
    );
}

/// 客户端显式声明的 stream_options 不被覆盖。
#[tokio::test]
async fn explicit_stream_options_survives() {
    let env = setup().await;
    let resp = post_chat(
        &env,
        json!({
            "model": env.model, "stream": true,
            "stream_options": {"include_usage": false},
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let upstream = env.seen.lock().unwrap().first().cloned().unwrap();
    assert_eq!(
        upstream.pointer("/stream_options/include_usage"),
        Some(&json!(false)),
        "客户端显式声明是其取舍，网关不改写"
    );
}
