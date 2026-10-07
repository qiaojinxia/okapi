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

#[path = "support/published_pricing.rs"]
mod published_pricing;

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

#[tokio::test]
#[allow(clippy::too_many_lines)] // Redirect and refusal cases share one billed video task.
async fn video_cdn_redirects_strip_credentials_and_reject_unsafe_or_looping_targets() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let observed = hits.clone();
    let cdn = Router::new().route(
        "/asset",
        get(move |headers: axum::http::HeaderMap| {
            let hits = observed.clone();
            async move {
                assert!(headers.get("authorization").is_none());
                assert!(headers.get("x-custom-secret").is_none());
                hits.fetch_add(1, Ordering::SeqCst);
                mock_content().await
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdn_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, cdn).await.unwrap();
    });
    let mode = Arc::new(AtomicUsize::new(0));
    let current = mode.clone();
    let upstream = Router::new().route("/v1/videos", post(mock_create)).route(
        "/v1/videos/video_mock123/content",
        get(move |headers: axum::http::HeaderMap| {
            let mode = current.clone();
            async move {
                assert!(headers.get("authorization").is_some());
                assert_eq!(headers["x-custom-secret"], "credential-extra");
                match mode.load(Ordering::SeqCst) {
                    0 => (
                        axum::http::StatusCode::FOUND,
                        [("location", format!("http://{cdn_addr}/asset"))],
                    )
                        .into_response(),
                    1 => (
                        axum::http::StatusCode::TEMPORARY_REDIRECT,
                        [("location", "/v1/videos/video_mock123/content")],
                    )
                        .into_response(),
                    _ => axum::http::StatusCode::FOUND.into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let env = setup(Money::from_micros(1_000_000), "/ok/v1").await;
    sqlx::query("UPDATE channels SET api_base=$1, settings=jsonb_build_object('extra_headers',jsonb_build_object('x-custom-secret','credential-extra')) WHERE name=$2")
        .bind(format!("http://{upstream_addr}/v1"))
        .bind(sqlx::query_scalar::<_, String>("SELECT name FROM channels WHERE models ? $1").bind(&env.model).fetch_one(&env.pg).await.unwrap())
        .execute(&env.pg).await.unwrap();
    sqlx::query("INSERT INTO settings(key,value) VALUES ('ssrf_policy',$1) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
        .bind(json!({"allow_http":true,"allow_private":true})).execute(&env.pg).await.unwrap();
    let client = reqwest::Client::new();
    let created = client
        .post(format!("http://{}/v1/videos", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.model,"prompt":"fixture","seconds":"4"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200);
    let download = format!("http://{}/v1/videos/video_mock123/content", env.gateway);
    let result = client
        .get(&download)
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    assert_eq!(result.headers()["content-type"], "video/mp4");
    assert_eq!(result.headers()["x-frame-options"], "DENY");
    assert_eq!(
        result.bytes().await.unwrap().as_ref(),
        &[0x66u8, 0x74, 0x79, 0x70]
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    sqlx::query("UPDATE settings SET value=$1 WHERE key='ssrf_policy'")
        .bind(json!({"allow_http":true,"allow_private":false}))
        .execute(&env.pg)
        .await
        .unwrap();
    let blocked = client
        .get(&download)
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), 502);
    assert_eq!(
        blocked.json::<Value>().await.unwrap()["error"]["param"],
        "video_download_redirect_target"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "rejected target must not receive a request"
    );
    sqlx::query("UPDATE settings SET value=$1 WHERE key='ssrf_policy'")
        .bind(json!({"allow_http":true,"allow_private":true}))
        .execute(&env.pg)
        .await
        .unwrap();
    mode.store(1, Ordering::SeqCst);
    let looping = client
        .get(&download)
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(looping.status(), 502);
    assert_eq!(
        looping.json::<Value>().await.unwrap()["error"]["param"],
        "video_download_redirect_limit"
    );
    mode.store(2, Ordering::SeqCst);
    let missing = client
        .get(&download)
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 502);
    assert_eq!(
        missing.json::<Value>().await.unwrap()["error"]["param"],
        "video_download_redirect_location"
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        960_000
    );
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/v1/videos", post(mock_create))
        .route("/ok/v1/videos/video_mock123", get(mock_poll))
        .route("/ok/v1/videos/video_mock123/content", get(mock_content))
        .route("/fail/v1/videos", post(mock_fail))
        .route("/laterfail/v1/videos", post(mock_create))
        .route("/stuck/v1/videos", post(mock_create))
        .route(
            "/stuck/v1/videos/video_mock123",
            get(|| async { axum::Json(json!({"id":"video_mock123","status":"in_progress"})) }),
        )
        .route(
            "/laterfail/v1/videos/video_mock123",
            get(|| async { axum::Json(json!({"id":"video_mock123","status":"failed"})) }),
        );
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
    state: gateway::state::AppState,
}

/// per_call 定价 0.01 USD/秒（micro=10000）。base_path: "/ok/v1" 或 "/fail/v1"。
async fn setup(balance: Money, base_path: &str) -> TestEnv {
    okapi_store::test_support::assert_isolated();
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
        None,
    )
    .await
    .unwrap();

    published_pricing::publish(&pg, user_id).await;
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    if !balance.is_zero() {
        state.ledger.credit(user_id, balance).await.unwrap();
    }
    let ledger = state.ledger.clone();

    let app = gateway::router(state.clone());
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
        state,
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
        let failed:i64=sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1 AND log_type=5 AND amount_micro=0 AND status=40").bind(env.user_id).fetch_one(&env.pg).await.unwrap();
        if balance.as_micros() == initial.as_micros() && failed == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("上游失败后余额应全额退回");
}

#[tokio::test]
async fn generation_failure_after_creation_refunds_exactly_once() {
    let initial = Money::from_micros(10_000_000);
    let env = setup(initial, "/laterfail/v1").await;
    let http = reqwest::Client::new();
    let created = http
        .post(format!("http://{}/v1/videos", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.model,"seconds":"4"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        9_960_000
    );
    for _ in 0..2 {
        let result = http
            .get(format!("http://{}/v1/videos/video_mock123", env.gateway))
            .bearer_auth(&env.token)
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), 200);
    }
    assert_eq!(env.ledger.balance(env.user_id).await.unwrap(), initial);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM billing_events WHERE user_id=$1 AND event_type='refund'",
    )
    .bind(env.user_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
async fn expired_video_and_interrupted_refund_close_once() {
    for claimed in [false, true] {
        let initial = Money::from_micros(10_000_000);
        // 过期分支：上游 25 小时后仍在生成
        let env = setup(initial, "/stuck/v1").await;
        let response = reqwest::Client::new()
            .post(format!("http://{}/v1/videos", env.gateway))
            .bearer_auth(&env.token)
            .json(&json!({"model":env.model,"prompt":"cat","seconds":"8"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        wait_committed(&env.pg, env.user_id, &env.model)
            .await
            .unwrap();
        if claimed {
            sqlx::query("UPDATE video_tasks SET state='refund_pending',channel_key_id=-1,next_poll_at=now() WHERE user_id=$1").bind(env.user_id).execute(&env.pg).await.unwrap();
        } else {
            sqlx::query("UPDATE video_tasks SET created_at=now()-interval '25 hours',next_poll_at=now() WHERE user_id=$1").bind(env.user_id).execute(&env.pg).await.unwrap();
        }
        gateway::videos::poll_pending(&env.state).await.unwrap();
        gateway::videos::poll_pending(&env.state).await.unwrap();
        assert_eq!(
            env.ledger.balance(env.user_id).await.unwrap(),
            initial,
            "timeout and interrupted refunds must return the original charge exactly once"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM video_tasks WHERE user_id=$1")
            .bind(env.user_id)
            .fetch_one(&env.pg)
            .await
            .unwrap();
        assert_eq!(state, "refunded");
    }
}

/// 过期退款之后上游才真正出片：退了钱就不再放行取片（按过期返回 404），也不回落 Redis
/// 映射；轮询照常透传状态，且不会把已退款改回完成。此前取片只查任务归属不查状态，等于钱退了、
/// 片子照拿。
#[tokio::test]
async fn refunded_video_can_no_longer_be_downloaded() {
    let initial = Money::from_micros(10_000_000);
    let env = setup(initial, "/stuck/v1").await;
    create_video(&env).await;
    let client = reqwest::Client::new();
    let fetch = |suffix: &'static str| {
        client
            .get(format!(
                "http://{}/v1/videos/video_mock123{suffix}",
                env.gateway
            ))
            .bearer_auth(&env.token)
            .send()
    };
    assert_eq!(fetch("").await.unwrap().status(), 200, "退款前照常轮询");
    assert_eq!(age_and_poll(&env, 25, None).await, "refunded");
    assert_eq!(env.ledger.balance(env.user_id).await.unwrap(), initial);
    // 上游事后完成、成片可取
    sqlx::query(
        "UPDATE channels SET api_base=replace(api_base,'/stuck/v1','/ok/v1') WHERE models ? $1",
    )
    .bind(&env.model)
    .execute(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        fetch("/content").await.unwrap().status(),
        404,
        "退款后不能再取片"
    );
    assert_eq!(fetch("").await.unwrap().status(), 200, "轮询照常透传状态");
    let state: String = sqlx::query_scalar("SELECT state FROM video_tasks WHERE user_id=$1")
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(state, "refunded", "上游事后完成不把已退款改回完成");
    assert_eq!(fetch("/content").await.unwrap().status(), 404);
}

async fn create_video(env: &TestEnv) {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/videos", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model":env.model,"prompt":"cat","seconds":"8"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    wait_committed(&env.pg, env.user_id, &env.model)
        .await
        .unwrap();
}

async fn age_and_poll(env: &TestEnv, hours: i32, channel_key: Option<i64>) -> String {
    sqlx::query(
        "UPDATE video_tasks SET created_at=now()-make_interval(hours=>$2),next_poll_at=now(),
         channel_key_id=COALESCE($3,channel_key_id) WHERE user_id=$1",
    )
    .bind(env.user_id)
    .bind(hours)
    .bind(channel_key)
    .execute(&env.pg)
    .await
    .unwrap();
    gateway::videos::poll_pending(&env.state).await.unwrap();
    sqlx::query_scalar("SELECT state FROM video_tasks WHERE user_id=$1")
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap()
}

/// 过了 24 小时才轮询到「已完成」：成片照常收费，不按超时退款。
/// 此前满 24 小时一律当失败退款，不看上游状态。
#[tokio::test]
async fn completed_video_past_the_deadline_is_not_refunded() {
    let initial = Money::from_micros(10_000_000);
    let env = setup(initial, "/ok/v1").await;
    create_video(&env).await;
    let charged = env.ledger.balance(env.user_id).await.unwrap();
    assert!(charged < initial);
    assert_eq!(age_and_poll(&env, 25, None).await, "completed");
    assert_eq!(env.ledger.balance(env.user_id).await.unwrap(), charged);
}

/// 查不到上游状态（渠道 key 已不可用）不等于失败：宽限到 72 小时才退款。
#[tokio::test]
async fn unreachable_video_status_is_refunded_only_after_the_give_up_deadline() {
    let initial = Money::from_micros(10_000_000);
    let env = setup(initial, "/ok/v1").await;
    create_video(&env).await;
    let charged = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(age_and_poll(&env, 25, Some(-1)).await, "pending");
    assert_eq!(env.ledger.balance(env.user_id).await.unwrap(), charged);
    assert_eq!(age_and_poll(&env, 73, Some(-1)).await, "refunded");
    assert_eq!(env.ledger.balance(env.user_id).await.unwrap(), initial);
}
