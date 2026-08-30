//! /v1/videos 异步任务面验收（IMPLEMENTATION §4.4 媒体计费）：
//! 提交 per_call×seconds 计费 / 任务轮询回源 / 成片流式下载 / 跨用户隔离 / 上游失败退款。
//! 依赖 .env 中的 DATABASE_URL 与 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

// ---- mock 上游 ----

async fn mock_create(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert!(req["model"].is_string(), "上游应收到 model 字段");
    axum::Json(json!({"id": "video_mock123", "object": "video", "status": "queued"}))
        .into_response()
}

async fn mock_poll() -> axum::response::Response {
    axum::Json(json!({"id": "video_mock123", "object": "video", "status": "completed"}))
        .into_response()
}

async fn mock_content() -> axum::response::Response {
    (
        [(axum::http::header::CONTENT_TYPE, "video/mp4")],
        vec![0x66u8, 0x74, 0x79, 0x70],
    )
        .into_response()
}

async fn mock_fail() -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({"error": {"message": "bad prompt"}})),
    )
        .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/v1/videos", post(mock_create))
        .route("/ok/v1/videos/video_mock123", get(mock_poll))
        .route("/ok/v1/videos/video_mock123/content", get(mock_content))
        .route("/fail/v1/videos", post(mock_fail));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

// ---- 测试环境 ----

struct TestEnv {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

/// per_call 定价 0.01 USD/秒（micro=10000）。base_path: "/ok/v1" 或 "/fail/v1"。
async fn setup(balance: Money, base_path: &str) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("vid-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-vid-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-vid")
        .await
        .unwrap();
    // per_call 定价：0.01 USD / 秒（媒体模型不配 ratio，与 audio stt 同口径）
    okapi_store::admin::upsert_model_per_call(&pg, &model, 10_000)
        .await
        .unwrap();

    let mock = spawn_mock().await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("vid-ch-{suffix}"),
        "openai",
        &format!("http://{mock}{base_path}"),
        "mock-credential",
        &[model.as_str()],
        false,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    if !balance.is_zero() {
        state.ledger.credit(user_id, balance).await.unwrap();
    }
    let ledger = state.ledger.clone();

    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        ledger,
        gateway: addr,
        token,
        user_id,
        model,
    }
}

async fn wait_committed(pg: &PgPool, user_id: i64, model: &str) -> Option<(i64, Option<Value>)> {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT amount_micro, pricing_snapshot
               FROM billing_records
               WHERE user_id = $1 AND model_name = $2 AND status = 20"#,
            user_id,
            model
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return Some((r.amount_micro, r.pricing_snapshot));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

// ---- 用例 ----

/// 提交 → per_call×seconds 计费 → 轮询 → 流式下载全链路。
#[tokio::test]
async fn videos_create_poll_download_bills_per_seconds() {
    let initial = Money::from_micros(10_000_000);
    let env = setup(initial, "/ok/v1").await;
    let http = reqwest::Client::new();

    // 提交（seconds="8" → 8 × 10000 micro = 80000）
    let resp = http
        .post(format!("http://{}/v1/videos", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "prompt": "a cat", "seconds": "8"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "video_mock123");

    let (amount, snapshot) = wait_committed(&env.pg, env.user_id, &env.model)
        .await
        .expect("提交应产生 committed 记录");
    assert_eq!(amount, 80_000, "0.01 USD/秒 × 8 秒 = 80000 micro");
    let units = snapshot
        .as_ref()
        .and_then(|s| s.get("media_units"))
        .and_then(Value::as_u64);
    assert_eq!(units, Some(8), "秒数应落 pricing_snapshot.media_units");

    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), initial.as_micros() - 80_000);

    // 轮询（不计费）
    let poll = http
        .get(format!("http://{}/v1/videos/video_mock123", env.gateway))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(poll.status(), 200);
    let poll_body: Value = poll.json().await.unwrap();
    assert_eq!(poll_body["status"], "completed");

    // 下载（流式透传）
    let dl = http
        .get(format!(
            "http://{}/v1/videos/video_mock123/content",
            env.gateway
        ))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    assert_eq!(
        dl.headers().get("content-type").unwrap(),
        "video/mp4",
        "content-type 应透传"
    );
    let bytes = dl.bytes().await.unwrap();
    assert_eq!(bytes.as_ref(), &[0x66u8, 0x74, 0x79, 0x70]);

    // 轮询/下载不追加计费
    let after = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(after.as_micros(), initial.as_micros() - 80_000);
}

/// 跨用户隔离：他人 key 轮询任务 → 404。
#[tokio::test]
async fn videos_task_isolated_across_users() {
    let env = setup(Money::from_micros(10_000_000), "/ok/v1").await;
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("http://{}/v1/videos", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "prompt": "a dog"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 同库另一用户
    let suffix = Uuid::new_v4().simple().to_string();
    let other_user = okapi_store::provision::create_user(&env.pg, &format!("u2-{suffix}"))
        .await
        .unwrap();
    let other_token = format!("sk-okapi-vid2-{suffix}");
    let other_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(other_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&env.pg, other_user, &other_hash, "sk-okapi-vid2")
        .await
        .unwrap();

    let poll = http
        .get(format!("http://{}/v1/videos/video_mock123", env.gateway))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(poll.status(), 404, "他人任务必须 404");
}

/// 上游 4xx：不计费全额退款。
#[tokio::test]
async fn videos_upstream_failure_refunds() {
    let initial = Money::from_micros(10_000_000);
    let env = setup(initial, "/fail/v1").await;
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("http://{}/v1/videos", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "prompt": "x", "seconds": "4"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502, "上游失败应报 upstream_error");

    // 退款后余额原样
    for _ in 0..50 {
        let balance = env.ledger.balance(env.user_id).await.unwrap();
        if balance.as_micros() == initial.as_micros() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("上游失败后余额应全额退回");
}
