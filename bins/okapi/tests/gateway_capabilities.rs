//! M3 能力感知路由验收（§3.8）：渠道 capabilities 显式 false 才排除；
//! tools/vision 请求特征探测。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::json;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

/// mock：记录每条路径的命中次数。
fn mock_router(hits_a: Arc<AtomicUsize>, hits_b: Arc<AtomicUsize>) -> Router {
    let ok = || {
        axum::Json(json!({
            "id":"cmpl","object":"chat.completion",
            "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}
        }))
        .into_response()
    };
    Router::new()
        .route(
            "/a/v1/chat/completions",
            post(move || {
                hits_a.fetch_add(1, Ordering::SeqCst);
                std::future::ready(ok())
            }),
        )
        .route(
            "/b/v1/chat/completions",
            post(move || {
                hits_b.fetch_add(1, Ordering::SeqCst);
                std::future::ready(ok())
            }),
        )
}

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    model: String,
    hits_a: Arc<AtomicUsize>,
    hits_b: Arc<AtomicUsize>,
}

/// 渠道 A：capabilities {"tools": false}；渠道 B：{}（未声明）。
async fn setup() -> TestEnv {
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

    let hits_a = Arc::new(AtomicUsize::new(0));
    let hits_b = Arc::new(AtomicUsize::new(0));
    let app = mock_router(Arc::clone(&hits_a), Arc::clone(&hits_b));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    for (path, caps) in [("/a/v1", json!({"tools": false})), ("/b/v1", json!({}))] {
        let (channel_id, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("ch{path}-{suffix}"),
            "openai",
            &format!("http://{mock}{path}"),
            "mock-credential",
            &[model.as_str()],
            false,
        )
        .await
        .unwrap();
        sqlx::query!(
            "UPDATE channels SET capabilities = $2 WHERE id = $1",
            channel_id,
            caps
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
        pg,
        gateway: addr,
        token,
        model,
        hits_a,
        hits_b,
    }
}

async fn post_chat(env: &TestEnv, with_tools: bool) -> reqwest::Response {
    // 每次唯一内容：避开 L2 会话粘性（否则全部粘在首个成功渠道）
    let mut body = json!({
        "model": env.model,
        "max_tokens": 16,
        "messages": [{"role":"user","content": format!("hi {}", Uuid::new_v4())}]
    });
    if with_tools {
        body["tools"] = json!([{"type": "function",
            "function": {"name": "f", "parameters": {"type": "object"}}}]);
    }
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// 带 tools 的请求永不落在显式 tools:false 的渠道；普通请求两渠道均可。
#[tokio::test]
async fn tools_requests_avoid_denying_channel() {
    let env = setup().await;
    for _ in 0..12 {
        let resp = post_chat(&env, true).await;
        assert_eq!(resp.status(), 200);
    }
    assert_eq!(
        env.hits_a.load(Ordering::SeqCst),
        0,
        "tools:false 渠道不得接到工具请求"
    );
    assert_eq!(env.hits_b.load(Ordering::SeqCst), 12);

    // 普通请求：A 渠道可参与（未被全局排除）
    for _ in 0..12 {
        let resp = post_chat(&env, false).await;
        assert_eq!(resp.status(), 200);
    }
    assert!(
        env.hits_a.load(Ordering::SeqCst) > 0,
        "无工具请求应能落在 A 渠道（缺声明≠排除）"
    );
    let _ = &env.pg;
}
