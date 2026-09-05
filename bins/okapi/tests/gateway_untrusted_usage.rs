//! 渠道 `trust_upstream_usage = false` 的本地复核：上游少报时按本地分词顶上去，
//! 多报时原样保留；trust=true 的渠道一律照单全收。
//!
//! 这个开关此前是死的——DB/API/UI 全通，gateway 从不读它，勾不勾行为一样。
//! 依赖 .env 的 DATABASE_URL 与 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::json;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

/// 上游谎报的 prompt_tokens：远低于 405 个汉字的真实分词量。
const UNDERREPORTED_PROMPT: u32 = 5;
const REPORTED_COMPLETION: u32 = 7;
/// 405 个汉字（真实分词约 300+）。
const PROMPT_UNIT: &str = "中文提示词内容测试";
const PROMPT_REPEAT: usize = 45;

async fn spawn_mock() -> SocketAddr {
    let handler = |_body: axum::body::Bytes| async move {
        axum::Json(json!({
            "id": "cmpl", "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "好的"}}],
            "usage": {
                "prompt_tokens": UNDERREPORTED_PROMPT,
                "completion_tokens": REPORTED_COMPLETION
            }
        }))
        .into_response()
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
}

async fn setup(trust_upstream_usage: bool) -> Env {
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
    let token = format!("sk-okapi-tu-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-tu")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        "openai",
        &format!("http://{mock}/up/v1"),
        "mock-credential",
        &[model.as_str()],
        trust_upstream_usage,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "tu-node", None, None)
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
    }
}

async fn chat_and_settle(env: &Env) -> (i32, i32) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "stream": false,
            "messages": [{"role": "user", "content": PROMPT_UNIT.repeat(PROMPT_REPEAT)}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let request_id = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap();
    let _ = resp.text().await.unwrap();

    for _ in 0..50 {
        if let Some(r) = sqlx::query!(
            r#"SELECT prompt_tokens, completion_tokens FROM billing_records
               WHERE request_id = $1 AND status = 20"#,
            request_id
        )
        .fetch_optional(&env.pg)
        .await
        .unwrap()
        {
            return (r.prompt_tokens, r.completion_tokens);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("billing_records 未在 5s 内出现");
}

/// trust=true：上游说多少就是多少（历史行为，不能变）。
#[tokio::test]
async fn trusted_channel_takes_upstream_usage_verbatim() {
    let env = setup(true).await;
    let (prompt, completion) = chat_and_settle(&env).await;
    assert_eq!(prompt, i32::try_from(UNDERREPORTED_PROMPT).unwrap());
    assert_eq!(completion, i32::try_from(REPORTED_COMPLETION).unwrap());
}

/// trust=false：少报的一侧被本地分词顶上去，多报的一侧原样保留。
#[tokio::test]
async fn untrusted_channel_recounts_the_underreported_side() {
    let env = setup(false).await;
    let (prompt, completion) = chat_and_settle(&env).await;
    assert!(
        prompt > i32::try_from(UNDERREPORTED_PROMPT).unwrap() * 10,
        "405 个汉字不可能只有 {UNDERREPORTED_PROMPT} tokens，本地复核应顶上去，实得 {prompt}"
    );
    assert_eq!(
        completion,
        i32::try_from(REPORTED_COMPLETION).unwrap(),
        "上游报的补全高于本地折算，应原样保留"
    );
}
