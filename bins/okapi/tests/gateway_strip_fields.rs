//! 渠道字段透传控制验收（new-api rc.23 #6847 对齐）：
//! 配置字段被剥、未配置字段照传、受保护键（model/messages/stream）不可剥、
//! 未配置渠道原样透传。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use uuid::Uuid;

type Captured = Arc<Mutex<Option<Value>>>;

async fn spawn_mock(captured: Captured) -> SocketAddr {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::body::Bytes| {
            let captured = Arc::clone(&captured);
            async move {
                let req: Value = serde_json::from_slice(&body).unwrap();
                *captured.lock().unwrap() = Some(req);
                axum::Json(json!({
                    "id":"cmpl","object":"chat.completion","model":"m",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
                    "usage":{"prompt_tokens":10,"completion_tokens":5,
                             "prompt_tokens_details":{"cached_tokens":0}}
                }))
                .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    gateway: SocketAddr,
    token: String,
    model: String,
    captured: Captured,
}

async fn setup(strip: Option<Value>) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("sf-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-sf-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-sf")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();

    let captured: Captured = Arc::new(Mutex::new(None));
    let mock = spawn_mock(Arc::clone(&captured)).await;
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("sf-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    if let Some(strip) = strip {
        sqlx::query!(
            r#"UPDATE channels
               SET settings = COALESCE(settings, '{}'::jsonb)
                   || jsonb_build_object('strip_request_fields', $2::jsonb)
               WHERE id = $1"#,
            channel_id,
            strip
        )
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
        gateway: addr,
        token,
        model,
        captured,
    }
}

async fn chat_with_extras(env: &TestEnv) -> Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "messages": [{"role": "user", "content": "hi"}],
            "logit_bias": {"50256": -100},
            "user": "end-user-1",
            "temperature": 0.5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();
    let captured = env.captured.lock().unwrap().clone();
    captured.expect("mock 应收到上游请求")
}

/// 配置剥除：logit_bias/user 不透传，temperature 照传，受保护键仍在。
#[tokio::test]
async fn strips_configured_fields_only() {
    let env = setup(Some(json!(["logit_bias", "user", "model", "messages"]))).await;
    let upstream = chat_with_extras(&env).await;
    assert!(
        upstream.get("logit_bias").is_none(),
        "配置字段应被剥：{upstream}"
    );
    assert!(upstream.get("user").is_none(), "配置字段应被剥");
    assert_eq!(upstream["temperature"], 0.5, "未配置字段照传");
    assert!(upstream.get("model").is_some(), "受保护键 model 不可剥");
    assert!(
        upstream.get("messages").is_some(),
        "受保护键 messages 不可剥"
    );
}

/// 未配置渠道：全部原样透传。
#[tokio::test]
async fn passes_through_without_config() {
    let env = setup(None).await;
    let upstream = chat_with_extras(&env).await;
    assert_eq!(upstream["logit_bias"]["50256"], -100, "缺省应原样透传");
    assert_eq!(upstream["user"], "end-user-1");
}
