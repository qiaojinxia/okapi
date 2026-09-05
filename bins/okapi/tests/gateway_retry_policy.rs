//! 渠道级 `retry_policy`：同 key 重试次数与首字窗口按渠道生效。
//!
//! 这一列此前只有 CRUD——存得进读得出，重试循环用的是硬编码 1 次 / 30 秒。
//! 依赖 .env 的 DATABASE_URL 与 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use uuid::Uuid;

/// 上游命中次数（数同 key 重试）。
type Hits = Arc<AtomicUsize>;

/// 前 N 次返回 500（瞬态），之后成功——用来数网关到底重试了几次。
async fn spawn_flaky_mock(hits: Hits, fail_first: usize) -> SocketAddr {
    let handler = move |_b: axum::body::Bytes| {
        let hits = Arc::clone(&hits);
        async move {
            let n = hits.fetch_add(1, Ordering::SeqCst);
            if n < fail_first {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(json!({"error": {"message": "boom"}})),
                )
                    .into_response();
            }
            axum::Json(json!({
                "id": "cmpl", "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5}
            }))
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
    gateway: SocketAddr,
    token: String,
    model: String,
    hits: Hits,
}

/// `policy`：写进 channels.retry_policy 的 JSON（None = 不配，走缺省）。
async fn setup(policy: Option<Value>, fail_first: usize) -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);

    let pg: PgPool = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-rp-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-rp")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let hits: Hits = Arc::new(AtomicUsize::new(0));
    let mock = spawn_flaky_mock(Arc::clone(&hits), fail_first).await;
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        "openai",
        &format!("http://{mock}/up/v1"),
        "mock-credential",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    if let Some(p) = policy {
        sqlx::query!(
            "UPDATE channels SET retry_policy = $2 WHERE id = $1",
            channel_id,
            p
        )
        .execute(&pg)
        .await
        .unwrap();
    }

    let state = gateway::build_state(&database_url, &redis_url, "rp-node", None, None)
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
        gateway: addr,
        token,
        model,
        hits,
    }
}

async fn chat(env: &Env) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "stream": false,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// 不配 retry_policy = 历史行为：同 key 只重试 1 次，共 2 次命中。
#[tokio::test]
async fn default_policy_retries_same_key_once() {
    let env = setup(None, 1).await;
    assert_eq!(chat(&env).await, 200, "第 2 次成功");
    assert_eq!(
        env.hits.load(Ordering::SeqCst),
        2,
        "缺省应为 1 次重试（首发 + 1 重试）"
    );
}

/// 配 same_key_retries=3：连挂三次仍能靠第四次救回来。
#[tokio::test]
async fn policy_raises_same_key_retry_budget() {
    let env = setup(Some(json!({"same_key_retries": 3})), 3).await;
    assert_eq!(chat(&env).await, 200);
    assert_eq!(
        env.hits.load(Ordering::SeqCst),
        4,
        "首发 + 3 次重试；缺省的 1 次重试到这里就该失败了"
    );
}

/// 配 same_key_retries=0：一次都不重试，首发失败即换渠道（此处无备渠道 → 失败）。
#[tokio::test]
async fn policy_can_disable_same_key_retry() {
    let env = setup(Some(json!({"same_key_retries": 0})), 1).await;
    assert_ne!(chat(&env).await, 200, "关掉重试后首发失败即出错");
    assert_eq!(env.hits.load(Ordering::SeqCst), 1, "只打了一次");
}

/// 越界值被夹取，不会让一个坏配置把请求吊死或彻底关掉重试。
#[tokio::test]
async fn out_of_range_policy_is_clamped() {
    // same_key_retries 上限 3：写 99 也只重试 3 次
    let env = setup(Some(json!({"same_key_retries": 99})), 9).await;
    assert_ne!(chat(&env).await, 200, "9 连挂超出夹取后的 3 次预算");
    assert_eq!(env.hits.load(Ordering::SeqCst), 4, "首发 + 夹到 3 次重试");
}

/// 首字窗口按渠道生效：配 5 秒的渠道不会等满 30 秒缺省窗口。
#[tokio::test]
async fn first_output_window_is_per_channel() {
    // 上游永不响应：连接建立后不发首字
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            // 收下连接但永不回应，逼网关走首字超时
            std::mem::forget(stream);
        }
    });

    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-rpw-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-rpw")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        "openai",
        &format!("http://{addr}/v1"),
        "mock-credential",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    // 5 秒窗口 + 不重试：总耗时应远低于 30 秒缺省
    sqlx::query!(
        "UPDATE channels SET retry_policy = $2 WHERE id = $1",
        channel_id,
        json!({"first_output_timeout_secs": 5, "same_key_retries": 0})
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "rpw-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let app = gateway::router(state);
    let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = l2.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l2, app).await.unwrap();
    });

    let t0 = std::time::Instant::now();
    let status = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .bearer_auth(&token)
        .json(&json!({
            "model": model, "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    let elapsed = t0.elapsed();
    assert_ne!(status, 200);
    assert!(
        elapsed < Duration::from_secs(20),
        "配了 5 秒窗口就不该等满 30 秒缺省，实耗 {elapsed:?}"
    );
}
