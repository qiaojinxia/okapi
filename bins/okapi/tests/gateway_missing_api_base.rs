//! openai_compat 没有缺省地址：漏填 api_base 的兼容渠道（老数据）永远连不上，网关跳过它——
//! 不回退到 api.openai.com，更不会把第三方的 key 发出去。有健康的备选渠道时请求照常成功。
//! 依赖 .env（scripts/dev-deps.sh up）。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

async fn spawn_mock(hits: Arc<AtomicUsize>) -> SocketAddr {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: axum::body::Bytes| {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
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
    hits: Arc<AtomicUsize>,
}

/// `healthy`：同一个模型是否再配一个能用的备选渠道。漏填地址的兼容渠道优先级更高，会先被考虑。
async fn setup(healthy: bool) -> TestEnv {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("mb-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-mb-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-mb")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();

    // 漏填地址：空串（导入或手改库留下的老数据）
    let (broken, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("mb-broken-{suffix}"),
        "openai_compat",
        "",
        "third-party-secret-key",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channels SET priority = 100 WHERE id = $1")
        .bind(broken)
        .execute(&pg)
        .await
        .unwrap();

    let hits = Arc::new(AtomicUsize::new(0));
    if healthy {
        let mock = spawn_mock(Arc::clone(&hits)).await;
        okapi_store::provision::create_channel(
            &pg,
            &format!("mb-good-{suffix}"),
            "openai",
            &format!("http://{mock}/v1"),
            "mock-credential",
            &[model.as_str()],
            true,
            None,
        )
        .await
        .unwrap();
    }

    published_pricing::publish(&pg, user_id).await;
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
        hits,
    }
}

async fn chat(env: &TestEnv) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

/// 只有一个漏填地址的兼容渠道：没有可用候选，请求被拒（503），而不是被发往别家的地址。
#[tokio::test]
async fn a_compat_channel_without_an_address_is_never_dialed() {
    let env = setup(false).await;
    let (status, body) = chat(&env).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "no_available_channel", "{body}");
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
}

/// 备选渠道健康时：漏填地址的渠道被跳过，请求由健康渠道照常服务。
#[tokio::test]
async fn a_healthy_channel_serves_when_the_compat_channel_has_no_address() {
    let env = setup(true).await;
    let (status, body) = chat(&env).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(env.hits.load(Ordering::SeqCst), 1, "只有健康渠道收到请求");
}
