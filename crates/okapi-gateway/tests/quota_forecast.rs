//! 订阅额度预判（观测 + 影子 / 强制）端到端：上游响应的限额头进诊断与预判、烧速计数、粘性时间戳、
//! 冷启动准入与保留线。mock 同时扮演 Anthropic 订阅上游与用量接口，不碰任何真实凭证。
//!
//! 场景刻意让结论只取决于「增量大于零」与「线的高低」，不依赖测试价簿的具体数字。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderName, HeaderValue},
    response::IntoResponse,
    routing::{get, post},
};
use fred::interfaces::{KeysInterface, LuaInterface};
use okapi_gateway::account_control::forecast::RequestShape;
use okapi_gateway::{self as gateway, account_control, state::AppState};
use okapi_kernel::sched_redis::session_hash;
use okapi_store::credential::OAuthCredential;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Mock {
    /// 回给 /v1/messages 的限额头（空 = 不带，像 API key 渠道）。
    limit_headers: Arc<Mutex<Vec<(String, String)>>>,
    messages: Arc<AtomicUsize>,
    /// /api/oauth/usage 报的 5 小时窗用量（整数百分点）。
    five_hour_used: Arc<AtomicU8>,
}

async fn messages(State(mock): State<Mock>) -> axum::response::Response {
    mock.messages.fetch_add(1, Ordering::SeqCst);
    let mut response = Json(json!({
        "id": "msg_forecast", "type": "message", "role": "assistant", "model": "claude-x",
        "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn",
        "usage": {"input_tokens": 100, "output_tokens": 20}
    }))
    .into_response();
    for (name, value) in mock.limit_headers.lock().unwrap().iter() {
        response.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    response
}

async fn usage(State(mock): State<Mock>) -> Json<Value> {
    Json(json!({
        "five_hour": {"utilization": mock.five_hour_used.load(Ordering::SeqCst),
                      "resets_at": "2099-01-01T00:00:00Z"},
        "seven_day": {"utilization": 10, "resets_at": "2099-01-01T00:00:00Z"},
    }))
}

struct Env {
    state: AppState,
    pg: PgPool,
    redis: fred::clients::Client,
    gateway: SocketAddr,
    mock: Mock,
    channel: i64,
    key: i64,
    user: i64,
    user_token: String,
    model: String,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn setup(account_control: Value) -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    // mock 在 127.0.0.1：连接时的 SSRF 校验要放开私网
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('ssrf_policy', '{\"allow_http\": true, \"allow_private\": true}')
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .execute(&pg)
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let user = okapi_store::provision::create_user(&pg, &format!("forecast-{suffix}"))
        .await
        .unwrap();
    let model = format!("forecast-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let user_token = format!("sk-okapi-forecast-{suffix}");
    let hash = hex::encode(Sha256::digest(user_token.as_bytes()));
    okapi_store::provision::create_api_key(&pg, user, &hash, "sk-okapi-for")
        .await
        .unwrap();

    let mock = Mock::default();
    let upstream = Router::new()
        .route("/v1/messages", post(messages))
        .route("/api/oauth/usage", get(usage))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });

    let credential = OAuthCredential {
        access_token: "forecast-access".into(),
        refresh_token: "forecast-refresh".into(),
        expires_at: now() + 3600,
        account_id: Some(suffix.clone()),
        account_label: None,
        scope: None,
    };
    let (channel, key) = okapi_store::provision::create_channel(
        &pg,
        &format!("forecast-{suffix}"),
        "anthropic_max",
        &format!("{base}/v1"),
        &credential.to_plaintext(),
        &[&model],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channel_keys SET credential_kind=1 WHERE id=$1")
        .bind(key)
        .execute(&pg)
        .await
        .unwrap();
    let env_state = {
        published_pricing::publish(&pg, user).await;
        gateway::build_state(&database, &redis_url, "forecast-test", None, None)
            .await
            .unwrap()
    };
    env_state
        .ledger
        .credit(user, okapi_domain::Money::from_micros(10_000_000))
        .await
        .unwrap();
    let router = gateway::router(env_state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let env = Env {
        state: env_state,
        pg,
        redis: okapi_store::connect_redis(&redis_url).await.unwrap(),
        gateway: gateway_addr,
        mock,
        channel,
        key,
        user,
        user_token,
        model,
    };
    set_control(&env, account_control).await;
    env
}

/// 改渠道的 `account_control` 并让路由缓存失效。
async fn set_control(env: &Env, control: Value) {
    sqlx::query("UPDATE channels SET settings = $2 WHERE id = $1")
        .bind(env.channel)
        .bind(json!({"account_control": control}))
        .execute(&env.pg)
        .await
        .unwrap();
    env.state.invalidate_routing_caches_local();
}

fn limit_headers(five_hour: f64, week: f64, reset: i64) -> Vec<(String, String)> {
    [
        ("anthropic-ratelimit-unified-status", "allowed".to_owned()),
        (
            "anthropic-ratelimit-unified-representative-claim",
            "five_hour".to_owned(),
        ),
        ("anthropic-ratelimit-unified-reset", reset.to_string()),
        (
            "anthropic-ratelimit-unified-5h-utilization",
            five_hour.to_string(),
        ),
        ("anthropic-ratelimit-unified-5h-reset", reset.to_string()),
        (
            "anthropic-ratelimit-unified-5h-status",
            "allowed".to_owned(),
        ),
        (
            "anthropic-ratelimit-unified-7d-utilization",
            week.to_string(),
        ),
        (
            "anthropic-ratelimit-unified-7d-reset",
            (reset + 400_000).to_string(),
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}

struct Reply {
    status: u16,
    request_id: Uuid,
    retry_after: Option<String>,
}

async fn message(env: &Env, session: &str) -> Reply {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .header("x-api-key", &env.user_token)
        .header("anthropic-version", "2023-06-01")
        .header("x-claude-code-session-id", session)
        .json(&json!({"model": env.model, "max_tokens": 64,
            "messages": [{"role": "user", "content": format!("hello {}", Uuid::new_v4())}]}))
        .send()
        .await
        .unwrap();
    Reply {
        status: response.status().as_u16(),
        request_id: response.headers()["x-okapi-request-id"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap(),
        retry_after: response
            .headers()
            .get("retry-after")
            .map(|v| v.to_str().unwrap().to_owned()),
    }
}

/// 这次请求落库的诊断（成功与失败的请求都有一行；结算在后台，轮询等它）。
async fn diagnostics(env: &Env, request_id: Uuid) -> Value {
    for _ in 0..80 {
        let found: Option<Value> = sqlx::query_scalar(
            "SELECT usage_details->'diagnostics' FROM billing_records WHERE request_id = $1",
        )
        .bind(request_id)
        .fetch_optional(&env.pg)
        .await
        .unwrap();
        if let Some(diagnostics) = found.filter(|d| !d.is_null()) {
            return diagnostics;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("请求诊断没有落库：{request_id}");
}

async fn redis_text(env: &Env, key: &str) -> Option<String> {
    env.redis.get::<Option<String>, _>(key).await.unwrap()
}

/// 等烧速计数追上结算（结算在后台任务里）。
async fn burn(env: &Env, at_least: i64) -> i64 {
    for _ in 0..80 {
        let value = redis_text(env, &format!("burn:ck:{}", env.key))
            .await
            .and_then(|text| text.parse::<i64>().ok())
            .unwrap_or(0);
        if value >= at_least {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("烧速计数没有累加");
}

/// 直接写一条响应头观测（像上游刚报过一样），基线取当前烧速计数。
async fn seed_header_observation(env: &Env, five_hour: f64, week: f64) {
    let burn_micro = redis_text(env, &format!("burn:ck:{}", env.key))
        .await
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(0);
    let at = now();
    let observation = json!({
        "at": at, "burn_micro": burn_micro,
        "windows": [
            {"window_secs": 18_000, "used": five_hour, "resets_at": at + 3_000},
            {"window_secs": 604_800, "used": week, "resets_at": at + 400_000},
        ]
    });
    let _: () = env
        .redis
        .set(
            format!("quota:hdr:ck:{}", env.key),
            observation.to_string(),
            Some(fred::types::Expiration::EX(3600)),
            None,
            false,
        )
        .await
        .unwrap();
}

async fn list_len(env: &Env, key: &str) -> i64 {
    env.redis
        .eval(
            "return redis.call('LLEN', KEYS[1])",
            vec![key.to_owned()],
            Vec::<String>::new(),
        )
        .await
        .unwrap()
}

fn session_key(env: &Env, session: &str) -> String {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-claude-code-session-id", session.parse().unwrap());
    let hash = session_hash(&headers, None, &[]).unwrap();
    format!("stick:sess:{{{}}}:v1:{hash}", env.user)
}

#[tokio::test]
async fn limit_headers_land_in_the_diagnostics_the_feed_the_burn_counter_and_the_sticky_clock() {
    let env = setup(json!({"refresh_mode": "external"})).await;
    let reset = now() + 3_000;
    *env.mock.limit_headers.lock().unwrap() = limit_headers(0.42, 0.08, reset);

    let reply = message(&env, "session-a").await;
    assert_eq!(reply.status, 200);
    let diagnostics = diagnostics(&env, reply.request_id).await;
    let note = &diagnostics["attempts"][0]["rate_limit"];
    assert_eq!(note["http"], 200);
    assert_eq!(note["claim"], "five_hour");
    assert_eq!(note["status"], "allowed");
    assert_eq!(note["w"][0]["name"], "five_hour");
    assert_eq!(note["w"][0]["utilization"], 0.42);
    assert_eq!(note["w"][0]["resets_at"], reset);
    assert_eq!(note["w"][1]["name"], "seven_day");

    // 后台喂预判：响应头观测、样本留存、θ 状态都在
    let mut header_observation = None;
    for _ in 0..80 {
        header_observation = redis_text(&env, &format!("quota:hdr:ck:{}", env.key)).await;
        if header_observation.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let observation: Value =
        serde_json::from_str(&header_observation.expect("响应头观测")).unwrap();
    assert!((observation["windows"][0]["used"].as_f64().unwrap() - 42.0).abs() < 1e-9);
    assert_eq!(observation["windows"][0]["window_secs"], 18_000);
    assert_eq!(
        list_len(&env, &format!("quota:hist:ck:{}", env.key)).await,
        1
    );

    // 烧速计数 = 这笔账记下的上游成本
    let cost: i64 =
        sqlx::query_scalar("SELECT upstream_cost_micro FROM billing_records WHERE request_id = $1")
            .bind(reply.request_id)
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert!(cost > 0);
    assert_eq!(burn(&env, cost).await, cost);

    // 粘性映射带着「上一次成功」的时刻
    let mapping = session_key(&env, "session-a");
    assert_eq!(
        redis_text(&env, &mapping).await.as_deref(),
        Some(env.key.to_string().as_str())
    );
    let at: i64 = redis_text(&env, &format!("{mapping}:at"))
        .await
        .expect("伴随键")
        .parse()
        .unwrap();
    assert!((now() - at).abs() < 30);
    let hit = env
        .state
        .sched
        .sticky_get(env.user, mapping.rsplit(':').next().unwrap())
        .await
        .expect("命中");
    assert_eq!((hit.key, hit.last_used), (env.key, Some(at)));
    assert!(hit.is_warm(now(), account_control::forecast::WARM_WINDOW_SECS));
    assert!(!hit.is_warm(now() + 600, account_control::forecast::WARM_WINDOW_SECS));
}

#[tokio::test]
async fn a_mapping_written_before_the_upgrade_still_routes_and_counts_as_warm() {
    let env = setup(json!({"refresh_mode": "external"})).await;
    // 升级前的映射：只有账号 id，没有伴随键
    let mapping = session_key(&env, "legacy-session");
    let _: () = env
        .redis
        .set(
            &mapping,
            env.key.to_string(),
            Some(fred::types::Expiration::EX(600)),
            None,
            false,
        )
        .await
        .unwrap();
    let hash = mapping.rsplit(':').next().unwrap().to_owned();
    let hit = env
        .state
        .sched
        .sticky_get(env.user, &hash)
        .await
        .expect("命中");
    assert_eq!((hit.key, hit.last_used), (env.key, None));
    assert!(
        hit.is_warm(now(), account_control::forecast::WARM_WINDOW_SECS),
        "没有时刻的旧映射保持此前的行为：按热算"
    );
    // 请求成功后补上时刻，旧版读同一个键仍然读得懂
    let reply = message(&env, "legacy-session").await;
    assert_eq!(reply.status, 200);
    diagnostics(&env, reply.request_id).await;
    assert!(redis_text(&env, &format!("{mapping}:at")).await.is_some());
    assert_eq!(
        redis_text(&env, &mapping).await.as_deref(),
        Some(env.key.to_string().as_str()),
        "映射本身的格式没变，回滚到旧版不会丢粘性"
    );
}

/// 轮询落观测时带上烧速基线；相邻两次轮询之间花了钱，θ 的估计就有了一对数据。
#[tokio::test]
async fn polls_keep_a_burn_baseline_and_teach_theta() {
    let env = setup(json!({"refresh_mode": "external"})).await;
    let row = okapi_store::oauth_credentials::target(&env.pg, env.channel, env.key)
        .await
        .unwrap()
        .unwrap();
    let plain = okapi_store::credential::open(None, &row.credential_ciphertext).unwrap();

    env.mock.five_hour_used.store(10, Ordering::SeqCst);
    account_control::quota::poll(&env.state, &row, &plain).await;
    let first: Value = serde_json::from_str(
        &redis_text(&env, &format!("quota:ck:{}", env.key))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["burn_micro"], 0, "还没花过钱：基线是 0，不是缺席");
    assert_eq!(
        list_len(&env, &format!("quota:hist:ck:{}", env.key)).await,
        1
    );

    // 两次轮询之间花了 $2，5 小时窗从 10% 涨到 16%（去掉取整偏差后恰好 6 个百分点）
    account_control::forecast::record_burn(&env.state, env.key, 2_000_000).await;
    env.mock.five_hour_used.store(16, Ordering::SeqCst);
    let _: i64 = env
        .redis
        .del(format!("quota:poll:ck:{}", env.key))
        .await
        .unwrap();
    account_control::quota::poll(&env.state, &row, &plain).await;
    let second: Value = serde_json::from_str(
        &redis_text(&env, &format!("quota:ck:{}", env.key))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(second["burn_micro"], 2_000_000);
    let theta: Value = serde_json::from_str(
        &redis_text(&env, &format!("quota:theta:ck:{}:18000", env.key))
            .await
            .expect("θ 状态"),
    )
    .unwrap();
    assert_eq!(theta["pairs"], 1);
    assert!(
        (theta["sy"].as_f64().unwrap() - 6.0).abs() < 1e-9,
        "{theta}"
    );
    assert!(
        (theta["sx"].as_f64().unwrap() - 2.0).abs() < 1e-9,
        "{theta}"
    );
    assert_eq!(
        list_len(&env, &format!("quota:hist:ck:{}", env.key)).await,
        2
    );
}

#[tokio::test]
async fn shadow_records_the_verdict_and_only_enforce_refuses_the_request() {
    let env = setup(json!({"refresh_mode": "external"})).await;
    // 5 小时窗已经 99.99%：任何非零增量都会越线
    seed_header_observation(&env, 99.99, 10.0).await;

    // 缺省 shadow：请求照常放行，诊断里留下「会被拦」的评估
    let reply = message(&env, "shadow-session").await;
    assert_eq!(reply.status, 200);
    let recorded = diagnostics(&env, reply.request_id).await;
    let forecast = &recorded["forecast"];
    assert_eq!(forecast["mode"], "shadow");
    assert!(forecast["tokens"].as_u64().unwrap() > 0);
    let candidate = &forecast["cands"][0];
    assert_eq!(candidate["k"], env.key);
    assert_eq!(candidate["v"], "block");
    assert_eq!(candidate["warm"], false, "新会话：冷启动");
    let five_hour = candidate["w"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["n"] == "five_hour")
        .unwrap();
    assert_eq!(five_hour["hit"], true);
    assert_eq!(five_hour["src"], "header");
    assert!(five_hour["u"].as_f64().unwrap() >= 99.99);
    assert!(five_hour["d"].as_f64().unwrap() > 0.0);
    assert_eq!(five_hour["line"], 100.0);
    assert_eq!(env.mock.messages.load(Ordering::SeqCst), 1);

    // 同样的状态，强制：不发给上游，没有可用渠道，并说明是预判排除的
    set_control(
        &env,
        json!({"refresh_mode": "external", "quota_forecast": "enforce"}),
    )
    .await;
    seed_header_observation(&env, 99.99, 10.0).await;
    let refused = message(&env, "enforce-session").await;
    assert_eq!(refused.status, 503);
    assert_eq!(
        env.mock.messages.load(Ordering::SeqCst),
        1,
        "被预判排除的请求不会打到上游"
    );
    let recorded = diagnostics(&env, refused.request_id).await;
    assert_eq!(recorded["excluded"][0]["reason"], "forecast");
    assert_eq!(recorded["excluded"][0]["rule"], "forecast");
    assert_eq!(recorded["excluded"][0]["window"], "five_hour");
    assert_eq!(recorded["excluded"][0]["limit_percent"], 100.0);
    assert_eq!(recorded["forecast"]["mode"], "enforce");
    assert!(
        refused
            .retry_after
            .as_deref()
            .and_then(|secs| secs.parse::<i64>().ok())
            .is_some_and(|secs| secs > 0),
        "告诉客户端最早什么时候再试：{:?}",
        refused.retry_after
    );

    // 关掉预判：评估不再写进诊断，也不拦
    set_control(
        &env,
        json!({"refresh_mode": "external", "quota_forecast": "off"}),
    )
    .await;
    let reply = message(&env, "off-session").await;
    assert_eq!(reply.status, 200);
    let recorded = diagnostics(&env, reply.request_id).await;
    assert!(recorded.get("forecast").is_none());
}

/// 放置线 90、保留线 100：窗口已经压到放置线之上时，缓存还热的会话继续，新会话被拒。
#[tokio::test]
async fn a_warm_session_runs_to_the_keep_line_while_a_new_one_stops_at_the_placement_line() {
    let control = json!({
        "refresh_mode": "external", "quota_forecast": "enforce",
        "quota_limits": {"18000": 90}, "quota_keep_limits": {"18000": 100},
    });
    let env = setup(control.clone()).await;

    // 窗口还有余量时，会话 S 正常建立（粘性映射 + 成功时刻）
    seed_header_observation(&env, 50.0, 10.0).await;
    let first = message(&env, "session-s").await;
    assert_eq!(first.status, 200);
    diagnostics(&env, first.request_id).await;

    // 窗口越过放置线（90.5 > 90）
    seed_header_observation(&env, 90.5, 10.0).await;
    let warm = message(&env, "session-s").await;
    assert_eq!(warm.status, 200, "缓存还热的会话用保留线，继续");
    let recorded = diagnostics(&env, warm.request_id).await;
    let candidate = &recorded["forecast"]["cands"][0];
    assert_eq!(candidate["warm"], true);
    assert_eq!(candidate["v"], "ok");
    let five_hour = candidate["w"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["n"] == "five_hour")
        .unwrap();
    assert_eq!(five_hour["line"], 100.0);

    let cold = message(&env, "session-new").await;
    assert_eq!(cold.status, 503, "新会话按放置线，被拦下");
    let recorded = diagnostics(&env, cold.request_id).await;
    assert_eq!(recorded["excluded"][0]["limit_percent"], 90.0);

    // 没有保留线时，同一个热会话也被放置线拦下——放行靠的就是保留线
    set_control(
        &env,
        json!({
            "refresh_mode": "external", "quota_forecast": "enforce",
            "quota_limits": {"18000": 90},
        }),
    )
    .await;
    seed_header_observation(&env, 90.5, 10.0).await;
    let strict = message(&env, "session-s").await;
    assert_eq!(strict.status, 503);

    // 缓存过期（超过 TTL）的会话不再算热：把成功时刻拨到十分钟前
    set_control(&env, control).await;
    seed_header_observation(&env, 90.5, 10.0).await;
    let mapping = session_key(&env, "session-s");
    let _: () = env
        .redis
        .set(
            format!("{mapping}:at"),
            (now() - 600).to_string(),
            Some(fred::types::Expiration::EX(600)),
            None,
            false,
        )
        .await
        .unwrap();
    let expired = message(&env, "session-s").await;
    assert_eq!(expired.status, 503, "缓存过期，按冷启动算");
}

/// 单个候选的评估不依赖 HTTP：直接对准入函数喂请求形状。
#[tokio::test]
async fn admission_assesses_cold_and_warm_candidates_with_their_own_increments() {
    let env = setup(json!({
        "refresh_mode": "external", "quota_forecast": "enforce",
        "quota_limits": {"18000": 90}, "quota_keep_limits": {"18000": 95},
    }))
    .await;
    seed_header_observation(&env, 80.0, 10.0).await;
    let candidates = okapi_store::channels::candidates_for_model(
        &env.pg,
        &env.model,
        &["default"],
        env.state.master_key.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(candidates.len(), 1);
    let assess = |warm_key: Option<i64>, cold_usd: f64, warm_usd: f64| {
        let mut pool = candidates.clone();
        let state = env.state.clone();
        async move {
            let shape = RequestShape {
                prompt_tokens: 300_000,
                cold_usd,
                warm_usd,
                warm_key,
            };
            account_control::retain_available(
                &state,
                &mut pool,
                account_control::QuotaOrder::Keep,
                Some(&shape),
            )
            .await;
            pool.len()
        }
    };
    // θ 先验 ≈ 3.03 %/$：冷启动 $4.5 → +13.6 个百分点，越过放置线 90
    assert_eq!(assess(None, 4.5, 0.1).await, 0, "冷启动会越线");
    // 同样的账号，几乎全读缓存的热请求（+0.3）远低于保留线 95
    assert_eq!(assess(Some(env.key), 4.5, 0.1).await, 1, "热请求放行");
    // 冷启动不大（+3）也越不过 90 − 80
    assert_eq!(assess(None, 1.0, 0.1).await, 1, "小请求放行");
    // 热会话在保留线上也有边：+3.03×6 越过 95 − 80
    assert_eq!(
        assess(Some(env.key), 4.5, 6.0).await,
        0,
        "热请求也不能越保留线"
    );
}

/// 让 mock 报这个 5 小时窗用量，并立刻轮询一次（先放掉轮询租约）。
async fn poll_with(env: &Env, row: &okapi_store::oauth_credentials::KeyRow, plain: &str, used: u8) {
    env.mock.five_hour_used.store(used, Ordering::SeqCst);
    let _: i64 = env
        .redis
        .del(format!("quota:poll:ck:{}", env.key))
        .await
        .unwrap();
    account_control::quota::poll(&env.state, row, plain).await;
}

/// 老的整数轮询路径上的双阈值要端到端成立：准入筛选、逐次尝试的闸，两处都认缓存还热的粘性账号。
#[tokio::test]
async fn the_polled_cap_honours_the_keep_line_for_warm_sessions_through_the_per_attempt_gate() {
    let control = json!({
        "refresh_mode": "external", "quota_forecast": "off",
        "quota_limits": {"18000": 80}, "quota_keep_limits": {"18000": 95},
    });
    let env = setup(control).await;
    let row = okapi_store::oauth_credentials::target(&env.pg, env.channel, env.key)
        .await
        .unwrap()
        .unwrap();
    let plain = okapi_store::credential::open(None, &row.credential_ciphertext).unwrap();
    poll_with(&env, &row, &plain, 50).await;
    let first = message(&env, "polled-session").await;
    assert_eq!(first.status, 200);
    diagnostics(&env, first.request_id).await;

    poll_with(&env, &row, &plain, 85).await;
    let warm = message(&env, "polled-session").await;
    assert_eq!(
        warm.status, 200,
        "85% 在放置线 80 之上、保留线 95 之下：热会话继续（准入与逐次尝试的闸都放行）"
    );
    let cold = message(&env, "polled-new-session").await;
    assert_eq!(cold.status, 503, "新会话按放置线");
    let recorded = diagnostics(&env, cold.request_id).await;
    assert_eq!(recorded["excluded"][0]["rule"], "window_limit");
    assert_eq!(recorded["excluded"][0]["limit_percent"], 80);

    // 热会话也不能越过保留线
    poll_with(&env, &row, &plain, 96).await;
    let over = message(&env, "polled-session").await;
    assert_eq!(over.status, 503);

    // 去掉保留线：同一个热会话在 85% 就被放置线挡下
    set_control(
        &env,
        json!({"refresh_mode": "external", "quota_forecast": "off", "quota_limits": {"18000": 80}}),
    )
    .await;
    poll_with(&env, &row, &plain, 85).await;
    let strict = message(&env, "polled-session").await;
    assert_eq!(strict.status, 503);
}

/// 单位不对的读数不能进预判：诊断里照实留着原始值（方便核对单位），预判的状态一概不写。
#[tokio::test]
async fn a_reading_in_the_wrong_unit_is_kept_in_the_diagnostics_but_never_fed_to_the_forecast() {
    let env = setup(json!({"refresh_mode": "external"})).await;
    // 5.0 能过解析（合理上界 10），折成百分点是 500%，超过 105 当单位不对
    *env.mock.limit_headers.lock().unwrap() = limit_headers(5.0, 0.08, now() + 3_000);
    let reply = message(&env, "wrong-unit").await;
    assert_eq!(reply.status, 200);
    let recorded = diagnostics(&env, reply.request_id).await;
    assert_eq!(
        recorded["attempts"][0]["rate_limit"]["w"][0]["utilization"], 5.0,
        "原始读数照实记"
    );
    let cost: i64 =
        sqlx::query_scalar("SELECT upstream_cost_micro FROM billing_records WHERE request_id = $1")
            .bind(reply.request_id)
            .fetch_one(&env.pg)
            .await
            .unwrap();
    burn(&env, cost).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 周窗 8% 是合理的，但同一次读数里有一个不合理的窗口——合理的那个照常进，不合理的整个丢弃
    let stored: Value = serde_json::from_str(
        &redis_text(&env, &format!("quota:hdr:ck:{}", env.key))
            .await
            .expect("周窗口读数合理，应当写入"),
    )
    .unwrap();
    let windows = stored["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1, "{stored}");
    assert_eq!(windows[0]["window_secs"], 604_800);
    assert!((windows[0]["used"].as_f64().unwrap() - 8.0).abs() < 1e-9);
    assert!(
        redis_text(&env, &format!("quota:theta:ck:{}:18000", env.key))
            .await
            .is_none(),
        "不合理的窗口不教 θ"
    );
}
