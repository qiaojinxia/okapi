//! M2 调度批次验收：模型别名解析 / L2 会话粘性 / key 级并发信号量 / 状态机分支。

#[path = "support/published_pricing.rs"]
mod published_pricing;

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

async fn mock_500(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"error":{"message":"upstream exploded","type":"server_error"}})),
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

/// Anthropic 的 529：服务级过载，不是这把 key 的问题。
async fn mock_529(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::from_u16(529).unwrap(),
        axum::Json(
            json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
        ),
    )
        .into_response()
}

/// Anthropic API key 预付费余额用完：HTTP 400 invalid_request_error，不是 402。
async fn mock_400_credit(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({"type":"error","error":{"type":"invalid_request_error",
            "message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}})),
    )
        .into_response()
}

/// 组织被停用：同样是 400。
async fn mock_400_org(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(
            json!({"type":"error","error":{"type":"invalid_request_error",
            "message":"This organization has been disabled."}}),
        ),
    )
        .into_response()
}

/// 普通的请求错误：问题在请求本身，换哪个渠道都一样。
async fn mock_400_plain(_body: axum::body::Bytes) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(
            json!({"error":{"message":"messages.0.content: Input should be a valid list",
            "type":"invalid_request_error"}}),
        ),
    )
        .into_response()
}

/// `/flip`：置位后像额度用完的账号一样回 429（只给粘性切换用例用）。
static FLIP_EXHAUSTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

async fn mock_flip(body: axum::body::Bytes) -> axum::response::Response {
    if FLIP_EXHAUSTED.load(std::sync::atomic::Ordering::SeqCst) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, "3600")],
            axum::Json(
                json!({"error":{"message":"usage limit reached","type":"rate_limit_error"}}),
            ),
        )
            .into_response();
    }
    mock_ok(body).await
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/v1/chat/completions", post(mock_ok))
        .route("/flip/v1/chat/completions", post(mock_flip))
        .route("/slow/v1/chat/completions", post(mock_slow))
        .route("/r429/v1/chat/completions", post(mock_429))
        .route("/r429q/v1/chat/completions", post(mock_429_quota))
        .route("/r401/v1/chat/completions", post(mock_401))
        .route("/r529/v1/chat/completions", post(mock_529))
        .route("/r400c/v1/chat/completions", post(mock_400_credit))
        .route("/r400o/v1/chat/completions", post(mock_400_org))
        .route("/r400p/v1/chat/completions", post(mock_400_plain))
        .route("/r500/v1/chat/completions", post(mock_500));
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
    okapi_store::test_support::assert_isolated();
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

    // 通配只认 `*`：模式里的 `_` / `%` 按字面，曾被 LIKE 当成单字符 / 任意串通配
    let literal = format!("lit_{}%", &suffix[..8]);
    sqlx::query("INSERT INTO model_aliases (pattern, target_model, priority) VALUES ($1, $2, 0)")
        .bind(format!("{literal}-*"))
        .bind(&env.model)
        .execute(&env.pg)
        .await
        .unwrap();
    let resp = post_chat(&env, &format!("{literal}-x"), None).await;
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();
    let resp = post_chat(&env, &format!("litX{}anything-x", &suffix[..8]), None).await;
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_ne!(status, 200, "`_` / `%` 不得当通配：{body}");
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

/// 粘住的 key 额度用完（429）：同一请求内换到别的 key，原 key 进冷却，会话改粘到新 key；
/// 原 key 恢复后会话也不切回去（不来回抖）。
#[tokio::test]
async fn sticky_key_exhausted_fails_over_and_rebinds() {
    // 高优先级的 flip 先被选中并粘住
    let env = setup(&[("/flip/v1", 10), ("/ok/v1", 0)]).await;
    let (exhausting, spare) = (env.channel_keys[0], env.channel_keys[1]);
    let session = format!("sess-{}", Uuid::new_v4().simple());
    let send = || async {
        let resp = post_chat(&env, &env.model, Some(&session)).await;
        assert_eq!(resp.status(), 200);
        let id = rid(&resp);
        let _ = resp.text().await.unwrap();
        wait_record(&env.pg, id).await
    };

    let first = send().await;
    assert_eq!(first.channel_key_id, Some(exhausting));
    assert_eq!(send().await.sticky_layer, 2, "已粘住");

    FLIP_EXHAUSTED.store(true, std::sync::atomic::Ordering::SeqCst);
    let switched = send().await;
    assert_eq!(
        switched.channel_key_id,
        Some(spare),
        "同一请求内换到备用 key"
    );
    let (status, cooling) = sqlx::query_as::<_, (i16, bool)>(
        "SELECT status, cooldown_until > now() + interval '50 minutes' FROM channel_keys WHERE id = $1",
    )
    .bind(exhausting)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!((status, cooling), (3, true), "按 Retry-After 冷却");

    let next = send().await;
    assert_eq!(next.channel_key_id, Some(spare));
    assert_eq!(next.sticky_layer, 2, "会话改粘到备用 key");

    // 原 key 恢复（冷却结束 + 不再 429）：会话留在备用 key 上
    FLIP_EXHAUSTED.store(false, std::sync::atomic::Ordering::SeqCst);
    sqlx::query("UPDATE channel_keys SET status = 1, cooldown_until = NULL WHERE id = $1")
        .bind(exhausting)
        .execute(&env.pg)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await; // 候选缓存 5s
    let after = send().await;
    assert_eq!(after.channel_key_id, Some(spare));
    assert_eq!(after.sticky_layer, 2);
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

/// 池里唯一的 key 在冷却：503 no_available_channel 带 `Retry-After`，指向它冷却结束的时刻，
/// 客户端不用盲目重试。
#[tokio::test]
async fn cooling_pool_reports_when_a_key_comes_back() {
    let env = setup(&[("/ok/v1", 0)]).await;
    sqlx::query(
        "UPDATE channel_keys SET status = 3, cooldown_until = now() + interval '120 seconds' WHERE id = $1",
    )
    .bind(env.channel_keys[0])
    .execute(&env.pg)
    .await
    .unwrap();

    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 503);
    let seconds: u64 = resp.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((100..=120).contains(&seconds), "Retry-After = {seconds}");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_available_channel", "{body}");
}

/// 真实的上游失败之后其余候选全忙：最终报那次真实失败（502，记在失败的渠道上），
/// 不能被"全忙"改写成 503 no_available_channel，把真实的 5xx 盖掉。
#[tokio::test]
async fn real_failure_is_reported_when_the_remaining_candidates_are_busy() {
    let env = setup(&[("/r500/v1", 10), ("/slow/v1", 0)]).await;
    sqlx::query!(
        "UPDATE channel_keys SET max_concurrency = 1 WHERE id = $1",
        env.channel_keys[1]
    )
    .execute(&env.pg)
    .await
    .unwrap();
    // 先到的请求在 r500 失败后改投慢渠道，占着它唯一的并发名额约 800ms
    let holder = post_chat(&env, &env.model, None);
    let probe = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        post_chat(&env, &env.model, None).await
    };
    let (held, resp) = tokio::join!(holder, probe);
    assert_eq!(held.status(), 200);
    let _ = held.text().await.unwrap();
    let status = resp.status();
    let id = rid(&resp);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 502, "{body}");
    assert_ne!(body["error"]["code"], "no_available_channel", "{body}");
    let rec = wait_record(&env.pg, id).await;
    assert_eq!(
        rec.channel_key_id,
        Some(env.channel_keys[0]),
        "失败记在真正出错的渠道上"
    );
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
    assert_eq!(resp.status(), 429, "上游限流的语义带到客户端，不再包成 502");
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

/// 一把 key 的 (status, failed_count, 是否在冷却)。运行时查询：不给 `.sqlx` 缓存添新条目。
async fn key_row(env: &TestEnv, key: i64) -> (i64, i64, bool) {
    sqlx::query_as(
        "SELECT status::bigint, failed_count::bigint, cooldown_until IS NOT NULL FROM channel_keys WHERE id = $1",
    )
    .bind(key)
    .fetch_one(&env.pg)
    .await
    .unwrap()
}

/// 账号级错误被上游包在 400 里（Anthropic 预付费余额用完、组织被停用）：请求本身没错，坏的是这把 key。
/// 当次改投下一个渠道并成功；这把 key 登记成 quota_exhausted（冷却到次日）/ invalid（仅人工恢复），
/// 否则它下一次还会被选中、再白白失败一次，落到它头上的那部分请求全部 400。
#[tokio::test]
async fn account_level_400_fails_over_and_registers_on_the_key() {
    for (path, status, cooling) in [("/r400c/v1", 4_i64, true), ("/r400o/v1", 6, false)] {
        let env = setup(&[(path, 10), ("/ok/v1", 0)]).await;
        let resp = post_chat(&env, &env.model, None).await;
        assert_eq!(
            served_by(&env, resp).await,
            env.channel_keys[1],
            "{path}: 应改投到好渠道"
        );
        let (key_status, failed, cooldown) = key_row(&env, env.channel_keys[0]).await;
        assert_eq!(key_status, status, "{path}");
        assert_eq!(failed, 1, "{path}");
        assert_eq!(cooldown, cooling, "{path}");
        let (healthy, _, _) = key_row(&env, env.channel_keys[1]).await;
        assert_eq!(healthy, 1, "{path}: 好 key 不受牵连");
    }
}

/// 对照：同样是 400，普通的请求错误仍是请求本身的问题——原样回给客户端，不换渠道，不动 key。
#[tokio::test]
async fn a_plain_400_still_reaches_the_client_and_leaves_the_key_alone() {
    let env = setup(&[("/r400p/v1", 10), ("/ok/v1", 0)]).await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body.to_string().contains("Input should be a valid list"),
        "上游原文透传：{body}"
    );
    let (key_status, failed, _) = key_row(&env, env.channel_keys[0]).await;
    assert_eq!((key_status, failed), (1, 0));
}

/// 池里每把 key 都试过且最后一次是上游限流：回 429，`Retry-After` 是最早有 key 回来的时刻（这里是
/// 上游给的 7 秒），日志里仍记真实的上游错误码与状态。此前一律 502，SDK 把它当网关故障重试。
#[tokio::test]
async fn exhausted_rate_limit_reaches_the_client_as_429_with_the_earliest_return() {
    let env = setup(&[("/r429/v1", 0)]).await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 429);
    let seconds: u64 = resp.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((4..=8).contains(&seconds), "Retry-After = {seconds}");
    let id = rid(&resp);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited", "{body}");
    assert_eq!(body["error"]["param"], "upstream", "{body}");
    let (error_code, upstream_status): (Option<String>, Option<i16>) = {
        let mut row = None;
        for _ in 0..50 {
            row = sqlx::query_as(
                "SELECT error_code, upstream_status FROM billing_records WHERE request_id = $1",
            )
            .bind(id)
            .fetch_optional(&env.pg)
            .await
            .unwrap();
            if row.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        row.expect("失败也要有记录")
    };
    assert_eq!(
        error_code.as_deref(),
        Some("upstream_status"),
        "日志记真实原因，不记对外的 rate_limited"
    );
    assert_eq!(upstream_status, Some(429));
}

/// 次数用尽（MAX_ATTEMPTS）而候选里还有没试过的 key：仍回 429，但不替客户端编一个等待时间。
#[tokio::test]
async fn rate_limit_with_untried_keys_left_has_no_retry_after() {
    let env = setup(&[
        ("/r429/v1", 0),
        ("/r429/v1", 0),
        ("/r429/v1", 0),
        ("/r429/v1", 0),
    ])
    .await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 429);
    assert!(
        !resp.headers().contains_key("retry-after"),
        "还有好 key 可能在后面，不该让客户端干等"
    );
    let untouched: i64 =
        sqlx::query_scalar("SELECT count(*) FROM channel_keys WHERE id = ANY($1) AND status = 1")
            .bind(&env.channel_keys)
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert_eq!(untouched, 1, "三次尝试冷却三把，第四把没碰");
}

/// 以最后一次失败为准：先 429 再 5xx 仍是 502（真实的最后一个故障），不被前面的限流带偏。
#[tokio::test]
async fn the_last_failure_decides_what_the_client_sees() {
    let env = setup(&[("/r429/v1", 10), ("/r500/v1", 0)]).await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 502);
    assert!(!resp.headers().contains_key("retry-after"));
}

/// 上游 529（服务级过载）：对客户端是 503，不是 502；key 状态机照旧按它登记。
#[tokio::test]
async fn upstream_overload_reaches_the_client_as_503() {
    let env = setup(&[("/r529/v1", 0)]).await;
    let resp = post_chat(&env, &env.model, None).await;
    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "upstream_status", "{body}");
}

/// 给令牌单独建一个池（全部渠道只在这个池里），选路策略由参数给出。
async fn pin_to_pool(env: &TestEnv, strategy: &str) {
    let pool = format!("p-{}", &Uuid::new_v4().simple().to_string()[..10]);
    okapi_store::admin::upsert_channel_pool(&env.pg, &pool, "t", strategy, None)
        .await
        .unwrap();
    for key in &env.channel_keys {
        let channel: i64 = sqlx::query_scalar("SELECT channel_id FROM channel_keys WHERE id = $1")
            .bind(key)
            .fetch_one(&env.pg)
            .await
            .unwrap();
        let member = okapi_store::admin::PoolMember {
            pool_code: pool.clone(),
            priority_override: None,
            weight_override: None,
        };
        okapi_store::admin::set_channel_pools(&env.pg, channel, &[member])
            .await
            .unwrap();
    }
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(env.token.as_bytes()))
    };
    sqlx::query("UPDATE api_keys SET pool_override = $1 WHERE key_hash = $2")
        .bind(&pool)
        .bind(&key_hash)
        .execute(&env.pg)
        .await
        .unwrap();
}

async fn redis() -> fred::clients::Client {
    okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap()
}

/// 某把 key 未到期的在途租约数（与网关读数同一口径）。
async fn in_flight(redis: &fred::clients::Client, key: i64) -> i64 {
    use fred::interfaces::SortedSetsInterface;
    #[allow(clippy::cast_precision_loss)]
    let now = chrono::Utc::now().timestamp_millis() as f64;
    redis
        .zcount(format!("conc:ck:{{{key}}}:v2"), now, f64::MAX)
        .await
        .unwrap()
}

async fn served_by(env: &TestEnv, resp: reqwest::Response) -> i64 {
    assert_eq!(resp.status(), 200);
    let id = rid(&resp);
    resp.text().await.unwrap();
    wait_record(&env.pg, id).await.channel_key_id.unwrap()
}

/// least_usage：同一层里当日消费多的 key 让路，请求都落到消费少的那把。
#[tokio::test]
async fn least_usage_pool_routes_to_the_key_with_less_spend_today() {
    use fred::interfaces::KeysInterface;
    let env = setup(&[("/ok/v1", 0), ("/ok/v1", 0)]).await;
    pin_to_pool(&env, "least_usage").await;
    let (heavy, light) = (env.channel_keys[0], env.channel_keys[1]);
    let day = chrono::Utc::now().format("%Y%m%d");
    let _: () = redis()
        .await
        .set(
            format!("spend:ck:{heavy}:{day}"),
            1_000_000_000_i64,
            Some(fred::types::Expiration::EX(600)),
            None,
            false,
        )
        .await
        .unwrap();
    for _ in 0..4 {
        let resp = post_chat(&env, &env.model, None).await;
        assert_eq!(served_by(&env, resp).await, light);
    }
}

/// least_concurrency：两把 key 都没设并发上限，第一个请求在途时第二个分到另一把；
/// 结束后两把 key 的在途都归零。
#[tokio::test]
async fn least_concurrency_pool_spreads_overlapping_requests() {
    let env = setup(&[("/slow/v1", 0), ("/slow/v1", 0)]).await;
    pin_to_pool(&env, "least_concurrency").await;
    let redis = redis().await;
    let first = tokio::spawn({
        let (gateway, token, model) = (env.gateway, env.token.clone(), env.model.clone());
        async move {
            reqwest::Client::new()
                .post(format!("http://{gateway}/v1/chat/completions"))
                .bearer_auth(token)
                .json(&json!({"model": model, "stream": true, "max_tokens": 32,
                    "messages": [{"role":"user","content":"first"}]}))
                .send()
                .await
                .unwrap()
        }
    });
    let mut busy = None;
    for _ in 0..200 {
        for key in &env.channel_keys {
            if in_flight(&redis, *key).await == 1 {
                busy = Some(*key);
            }
        }
        if busy.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let busy = busy.expect("没设上限的 key 也应登记在途");
    let second = post_chat(&env, &env.model, None).await;
    assert_ne!(served_by(&env, second).await, busy);
    assert_eq!(served_by(&env, first.await.unwrap()).await, busy);
    for _ in 0..50 {
        let mut total = 0;
        for key in &env.channel_keys {
            total += in_flight(&redis, *key).await;
        }
        if total == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("请求结束后在途租约应退回");
}
