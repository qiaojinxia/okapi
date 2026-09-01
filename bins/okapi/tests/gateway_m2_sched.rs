//! M2 调度批次验收：模型别名解析 / L2 会话粘性 / key 级并发信号量 / 状态机分支。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

fn sse(chunks: &[Value]) -> axum::response::Response {
    use std::fmt::Write as _;
    let mut body = String::new();
    for c in chunks {
        let _ = write!(body, "data: {c}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn chunks() -> Vec<Value> {
    vec![
        json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
        json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":0}}}),
    ]
}

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    sse(&chunks())
}

async fn mock_slow(_body: axum::body::Bytes) -> axum::response::Response {
    tokio::time::sleep(Duration::from_millis(800)).await;
    sse(&chunks())
}

async fn mock_429(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, "7")],
        axum::Json(json!({"error":{"message":"rate limited","type":"rate_limit_error"}})),
    )
        .into_response()
}

async fn mock_429_quota(_body: axum::body::Bytes) -> axum::response::Response {
    // OpenAI 风格：429 状态但语义是配额耗尽 → 必须归类 quota_exhausted（冷却到次日）
    (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        axum::Json(json!({"error":{"message":"You exceeded your current quota",
            "type":"insufficient_quota","code":"insufficient_quota"}})),
    )
        .into_response()
}

async fn mock_401(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error":{"message":"bad key","type":"invalid_request_error"}})),
    )
        .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/v1/chat/completions", post(mock_ok))
        .route("/slow/v1/chat/completions", post(mock_slow))
        .route("/r429/v1/chat/completions", post(mock_429))
        .route("/r429q/v1/chat/completions", post(mock_429_quota))
        .route("/r401/v1/chat/completions", post(mock_401));
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
    model: String,
    channel_keys: Vec<i64>,
}

async fn setup(channels: &[(&str, i32)]) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-s-{}", &suffix[..10]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("s-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-sched-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-sch")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    let mut channel_keys = Vec::new();
    for (i, (path, priority)) in channels.iter().enumerate() {
        let base = format!("http://{mock}{path}");
        let (channel_id, key_id) = okapi_store::provision::create_channel(
            &pg,
            &format!("s{i}-{suffix}"),
            "openai",
            &base,
            "mock-credential",
            &[model.as_str()],
            false,
            None,
        )
        .await
        .unwrap();
        sqlx::query!(
            "UPDATE channels SET priority = $2 WHERE id = $1",
            channel_id,
            priority
        )
        .execute(&pg)
        .await
        .unwrap();
        channel_keys.push(key_id);
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
        channel_keys,
    }
}

async fn post_chat(env: &TestEnv, model: &str, session: Option<&str>) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": model,
            "stream": true,
            "max_tokens": 32,
            "messages": [{"role":"user","content":"hello sched"}]
        }));
    if let Some(s) = session {
        req = req.header("x-session-id", s);
    }
    req.send().await.unwrap()
}

#[derive(Debug)]
struct Rec {
    model_name: String,
    channel_key_id: Option<i64>,
    sticky_layer: i16,
    #[allow(dead_code)] // 调试输出用
    status: i16,
}

async fn wait_record(pg: &PgPool, request_id: Uuid) -> Rec {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT model_name, channel_key_id, sticky_layer, status
               FROM billing_records WHERE request_id = $1"#,
            request_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return Rec {
                model_name: r.model_name,
                channel_key_id: r.channel_key_id,
                sticky_layer: r.sticky_layer,
                status: r.status,
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("billing_records 未出现 request_id={request_id}");
}

fn rid(resp: &reqwest::Response) -> Uuid {
    resp.headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .expect("缺少 x-okapi-request-id")
}

/// 别名解析：精确别名与通配别名都路由到 canonical 并按 canonical 记账。
#[tokio::test]
async fn model_alias_resolves_to_canonical() {
    let env = setup(&[("/ok/v1", 0)]).await;
    let suffix = Uuid::new_v4().simple().to_string();
    let exact = format!("alias-{}", &suffix[..8]);
    let wild_prefix = format!("wild-{}", &suffix[..8]);

    sqlx::query!(
        r#"INSERT INTO model_aliases (pattern, target_model, priority) VALUES ($1, $2, 0), ($3, $2, 0)"#,
        exact,
        env.model,
        format!("{wild_prefix}-*")
    )
    .execute(&env.pg)
    .await
    .unwrap();

    // 精确别名
    let resp = post_chat(&env, &exact, None).await;
    assert_eq!(resp.status(), 200);
    let id = rid(&resp);
    let _ = resp.text().await.unwrap();
    let rec = wait_record(&env.pg, id).await;
    assert_eq!(rec.model_name, env.model, "按 canonical 记账");

    // 通配别名
    let resp = post_chat(&env, &format!("{wild_prefix}-preview"), None).await;
    assert_eq!(resp.status(), 200);
    let id = rid(&resp);
    let _ = resp.text().await.unwrap();
    let rec = wait_record(&env.pg, id).await;
    assert_eq!(rec.model_name, env.model);
}

/// L2 会话粘性：同 session 第二次请求命中同一 channel_key，sticky_layer=2。
#[tokio::test]
async fn session_sticky_hits_same_channel_key() {
    let env = setup(&[("/ok/v1", 0), ("/ok/v1", 0)]).await;
    let session = format!("sess-{}", Uuid::new_v4().simple());

    let resp1 = post_chat(&env, &env.model, Some(&session)).await;
    assert_eq!(resp1.status(), 200);
    let id1 = rid(&resp1);
    let _ = resp1.text().await.unwrap();
    let rec1 = wait_record(&env.pg, id1).await;

    let resp2 = post_chat(&env, &env.model, Some(&session)).await;
    assert_eq!(resp2.status(), 200);
    let id2 = rid(&resp2);
    let _ = resp2.text().await.unwrap();
    let rec2 = wait_record(&env.pg, id2).await;

    assert_eq!(
        rec1.channel_key_id, rec2.channel_key_id,
        "同会话应命中同一渠道 key"
    );
    assert_eq!(rec2.sticky_layer, 2, "第二次应为 session 层命中");
}

/// key 级并发信号量：max_concurrency=1 时并发第二请求被拒（503），完成后恢复。
#[tokio::test]
async fn key_concurrency_slot_limits_inflight() {
    let env = setup(&[("/slow/v1", 0)]).await;
    sqlx::query!(
        "UPDATE channel_keys SET max_concurrency = 1 WHERE id = $1",
        env.channel_keys[0]
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let (r1, r2) = tokio::join!(
        post_chat(&env, &env.model, None),
        post_chat(&env, &env.model, None)
    );
    let mut statuses = [r1.status().as_u16(), r2.status().as_u16()];
    statuses.sort_unstable();
    // 一个成功（200），一个因唯一候选并发满被拒（503 no_available_channel）
    assert_eq!(statuses, [200, 503], "并发上限必须生效");
    // 排空成功流，等待信号量释放
    for r in [r1, r2] {
        if r.status() == 200 {
            let _ = r.text().await;
        }
    }

    let mut ok = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let r = post_chat(&env, &env.model, None).await;
        if r.status() == 200 {
            let _ = r.text().await;
            ok = true;
            break;
        }
    }
    assert!(ok, "完成后信号量应释放，请求恢复可用");
}

/// 429 + insufficient_quota body：语义为配额耗尽 → quota_exhausted（冷却到次日），
/// 不得误归 rate_limited（审计项 5b 回归防护）。
#[tokio::test]
async fn quota_exhausted_beats_rate_limited_classification() {
    let env = setup(&[("/r429q/v1", 0)]).await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 502);
    let row = sqlx::query!(
        r#"SELECT status, cooldown_until FROM channel_keys WHERE id = $1"#,
        env.channel_keys[0]
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(row.status, 4, "insufficient_quota 应转 quota_exhausted");
    let cooldown = row.cooldown_until.expect("应冷却到次日 0 点");
    let hours = (cooldown - chrono::Utc::now()).num_minutes();
    assert!(
        hours > 0 && hours <= 24 * 60,
        "冷却应在次日 0 点（UTC），got {hours}min"
    );
}

/// 状态机分支：429 → rate_limited（Retry-After 冷却）；401 → invalid（仅人工）。
#[tokio::test]
async fn key_state_machine_branches() {
    // 429
    let env = setup(&[("/r429/v1", 0)]).await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 502);
    let row = sqlx::query!(
        r#"SELECT status, cooldown_until, last_error FROM channel_keys WHERE id = $1"#,
        env.channel_keys[0]
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(row.status, 3, "429 应转 rate_limited");
    let cooldown = row.cooldown_until.expect("应有冷却期");
    let secs = (cooldown - chrono::Utc::now()).num_seconds();
    assert!(
        (4..=8).contains(&secs),
        "冷却应约等于 Retry-After=7，got {secs}"
    );

    // 401
    let env = setup(&[("/r401/v1", 0)]).await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 502);
    let row = sqlx::query!(
        r#"SELECT status, cooldown_until FROM channel_keys WHERE id = $1"#,
        env.channel_keys[0]
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(row.status, 6, "401 应转 invalid（仅人工恢复）");
    assert!(row.cooldown_until.is_none());
}
