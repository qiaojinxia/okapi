//! 负毛利自动熔断（IMPLEMENTATION §11.34）：worker 评估 → Redis `mb:blocks` → 网关候选过滤
//! → 管理面列出 / 解除 → 解除期内不再熔断 → 关闭功能清表。
//! 依赖 .env 与 ClickHouse（scripts/dev-deps.sh up）；未配 CH 时跳过。

use axum::response::IntoResponse;
use axum::routing::post;
use okapi::worker::{chsink, margin_breaker};
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":10,"completion_tokens":2}
    }))
    .into_response()
}

struct Env {
    pg: PgPool,
    redis: fred::clients::Client,
    state: gateway::state::AppState,
    gateway: SocketAddr,
    console: SocketAddr,
    admin_token: String,
    user_token: String,
    user_id: i64,
    key_id: i64,
    model: String,
    group: String,
    channel_id: i64,
}

// 用户 / 分组 / 渠道 / 三个服务的装配放同一视野
#[allow(clippy::too_many_lines)]
async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("mb-m-{}", &suffix[..10]);
    let group = format!("mb-g-{}", &suffix[..10]);
    okapi_store::admin::upsert_price_group(
        &pg,
        okapi_store::admin::PriceGroupInput {
            group_code: &group,
            group_ratio: "1",
            description: "",
            pool_code: None,
            self_select: false,
            rpm_limit: None,
            rph_limit: None,
        },
    )
    .await
    .unwrap();

    let admin_id = okapi_store::provision::create_user(&pg, &format!("mba-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-mba-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-mba")
        .await
        .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("mbu-{suffix}"))
        .await
        .unwrap();
    okapi_store::admin::set_user_groups(&pg, user_id, &[(group.clone(), 10)])
        .await
        .unwrap();
    let user_token = format!("sk-okapi-mbu-{suffix}");
    let key_id = okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-mbu")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock_app = axum::Router::new().route("/v1/chat/completions", post(mock_ok));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_app).await.unwrap();
    });
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("mb-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let gw = gateway::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            gw.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let cs = console::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, cs).await.unwrap();
    });

    Env {
        pg,
        redis,
        state,
        gateway: gateway_addr,
        console: console_addr,
        admin_token,
        user_token,
        user_id,
        key_id,
        model,
        group,
        channel_id,
    }
}

/// 成本已知、每笔收 amount 付 cost 的一条日志载荷。
fn loss_payload(env: &Env, amount: i64, cost: i64) -> Value {
    json!({
        "request_id": Uuid::new_v4(),
        "user_id": env.user_id,
        "api_key_id": env.key_id,
        "group": env.group,
        "model": env.model,
        "channel_id": env.channel_id,
        "channel_key_id": 1,
        "log_type": 2,
        "prompt_tokens": 100,
        "cached_tokens": 0,
        "completion_tokens": 50,
        "reasoning_tokens": 0,
        "amount_micro": amount,
        "original_amount_micro": amount,
        "discount_micro": 0,
        "upstream_cost_micro": cost,
        "upstream_cost_known": true,
        "pricing_epoch": 1,
        "latency_ms": 500,
        "ttft_ms": 50,
        "is_stream": false,
        "sticky_layer": 0,
        "failover_count": 0,
        "error_code": "",
        "node": "test-node",
        "client_type": "test",
    })
}

/// 喂 outbox → 排空到 CH → 轮询到立方体里能看见这对为止（outbox 是全局队列，并行用例持锁时
/// 一次 drain 会提前收敛）。
async fn seed_and_wait(env: &Env, ch: &okapi_store::ChClient, rows: usize) {
    let payloads: Vec<Value> = (0..rows).map(|_| loss_payload(env, 100, 300)).collect();
    sqlx::query!(
        r#"INSERT INTO billing_outbox (topic, payload)
           SELECT 'request_log', p FROM UNNEST($1::jsonb[]) AS p"#,
        &payloads
    )
    .execute(&env.pg)
    .await
    .unwrap();
    ch.ensure_schema().await.unwrap();
    for _ in 0..100 {
        while chsink::process_once(&env.pg, ch).await.unwrap() > 0 {}
        let seen = ch
            .query_with_params(
                "SELECT countIfMerge(cost_known) AS known FROM mv_analysis_hour \
                 WHERE group_code = {g:String} AND channel_id = {c:UInt32}",
                &[
                    ("g", env.group.as_str()),
                    ("c", &env.channel_id.to_string()),
                ],
            )
            .await
            .unwrap();
        let known = seen
            .first()
            .and_then(|r| r["known"].as_str())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        if known >= rows {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("立方体里始终看不到熔断样本");
}

async fn set_breaker(env: &Env, value: Value) {
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('margin_breaker', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        value
    )
    .execute(&env.pg)
    .await
    .unwrap();
    env.state.settings_cache.invalidate_all();
}

async fn chat(env: &Env) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({"model": env.model, "max_tokens": 16,
            "messages": [{"role":"user","content": format!("q-{}", Uuid::new_v4())}]}))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// 收告警的 mock webhook：返回 (地址, 收到的包络列表)。
async fn spawn_notify_sink() -> (SocketAddr, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&got);
    let app = axum::Router::new().route(
        "/hook",
        post(move |axum::Json(v): axum::Json<Value>| {
            let sink = std::sync::Arc::clone(&sink);
            async move {
                sink.lock().unwrap().push(v);
                axum::Json(json!({"ok": true}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, got)
}

/// 用完即删的临时库，只为承载 `settings.notify_channels`。
struct TempDb {
    pool: PgPool,
    admin: PgPool,
    name: String,
}

impl TempDb {
    async fn teardown(self) {
        self.pool.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
            self.name
        )))
        .execute(&self.admin)
        .await;
    }
}

/// 订阅 `margin_breaker` 的 Notifier。配置放临时库：`notify_channels` 是全局键，
/// 写在共享开发库上会与 `worker_notify` 的用例互相覆盖（§2.4 记的就是这条）。
/// 静默键在 Redis 里且按事件名固定，先 DEL 掉，否则上一次跑留下的键会把本轮吞掉。
async fn notify_via_temp_db(sink: &SocketAddr) -> (okapi::worker::notify::Notifier, TempDb) {
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let admin = okapi_store::connect_pg(&database_url).await.unwrap();
    let name = format!("okapi_mbn_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!(r#"CREATE DATABASE "{name}""#)))
        .execute(&admin)
        .await
        .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let pool = okapi_store::connect_pg(&format!("{base}/{name}"))
        .await
        .unwrap();
    okapi_store::run_migrations(&pool).await.unwrap();
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('notify_channels', $1)"#,
        json!([{ "type": "webhook", "url": format!("http://{sink}/hook"),
                 "events": ["margin_breaker"], "min_interval_secs": 1 }])
    )
    .execute(&pool)
    .await
    .unwrap();

    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let _: Option<i64> =
        fred::interfaces::KeysInterface::del(&redis, "notify:mute:0:margin_breaker")
            .await
            .unwrap();
    let notifier = okapi::worker::notify::Notifier::new(pool.clone(), redis);
    (notifier, TempDb { pool, admin, name })
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn breaker_trips_blocks_lists_lifts_and_clears() {
    let env = setup().await;
    let Some(ch) = env.state.ch.clone() else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    };
    // 25 笔每笔收 100 付 300：亏 200%
    seed_and_wait(&env, &ch, 25).await;
    set_breaker(
        &env,
        json!({"enabled": true, "window_hours": 24, "min_requests": 20,
               "min_cost_micro": 1000, "margin_bp": 0, "cooldown_secs": 600, "lift_secs": 600}),
    )
    .await;
    // 未熔断前请求可达
    assert_eq!(chat(&env).await.0, 200);

    // 走生产那条路（evaluate + 派发同一函数），并在 mock sink 上核对真实载荷：
    // 订阅配置读的是 Notifier 自己那个池，所以挂在临时库上，`settings.notify_channels`
    // 这个全局键不会和 `worker_notify::notify_dispatch_and_mute` 互相覆盖（§2.4 挂账的缺口）。
    let (hook, alerts) = spawn_notify_sink().await;
    let notifier = notify_via_temp_db(&hook).await;
    let now = chrono::Utc::now();
    let report =
        margin_breaker::evaluate_and_notify(&env.pg, Some(&ch), &env.redis, now, &notifier.0)
            .await
            .unwrap();
    let tripped = report
        .tripped
        .iter()
        .find(|t| t.group_code == env.group && t.channel_id == env.channel_id)
        .expect("本用例的分组×渠道应被熔断");
    assert_eq!(tripped.requests, 25);
    assert_eq!(tripped.amount_micro, 2_500);
    assert_eq!(tripped.cost_micro, 7_500);
    assert_eq!(tripped.margin_bp, -20_000);

    // 载荷形状：告警要能直接看出"哪个分组打哪个渠道、亏了多少"，
    // 只发一个 blocked_total 的话收告警的人还得自己回站上查
    let sent = alerts.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "本轮新熔断应恰好发一条：{sent:?}");
    assert_eq!(sent[0]["event"], "margin_breaker");
    assert!(sent[0]["at"].is_string(), "包络要带时间戳");
    let payload = &sent[0]["payload"];
    assert_eq!(
        payload["blocked_total"], report.blocked_total,
        "总数要与报告一致：{payload}"
    );
    let mine = payload["tripped"]
        .as_array()
        .expect("tripped 必须是数组")
        .iter()
        .find(|t| t["group_code"] == env.group.as_str())
        .expect("本用例的对应出现在载荷里");
    assert_eq!(mine["channel_id"], env.channel_id);
    assert_eq!(mine["requests"], 25);
    assert_eq!(mine["amount_micro"], 2_500);
    assert_eq!(mine["cost_micro"], 7_500);
    assert_eq!(mine["margin_bp"], -20_000);

    // 网关：该分组的唯一候选被摘掉 → 503 margin_blocked（不是 no_available_channel）
    env.state.margin_cache.invalidate_all();
    let (status, body) = chat(&env).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "margin_blocked");

    // 续期不算新增（第二轮 tripped 为空），而且这回是在 sink 上验的：没有第二条告警发出去
    let again =
        margin_breaker::evaluate_and_notify(&env.pg, Some(&ch), &env.redis, now, &notifier.0)
            .await
            .unwrap();
    assert!(
        !again.tripped.iter().any(|t| t.group_code == env.group),
        "仍在熔断中的对不重复通知"
    );
    assert_eq!(
        alerts.lock().unwrap().len(),
        1,
        "续期轮不得再吵一次（这条此前只在报告里验，没验到线上）"
    );
    notifier.1.teardown().await;

    // 管理面列出
    let client = reqwest::Client::new();
    let list: Value = client
        .get(format!("http://{}/admin/margin-breaker", env.console))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["config"]["enabled"], true);
    let row = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["group_code"] == env.group && r["channel_id"] == env.channel_id)
        .expect("列表应含本对");
    assert_eq!(row["state"], "blocked");
    assert_eq!(row["active"], true);
    assert!(row["channel_name"].as_str().unwrap().starts_with("mb-ch-"));

    // 解除：立刻放行，且解除期内评估器跳过
    let lifted = client
        .post(format!("http://{}/admin/margin-breaker/lift", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"group_code": env.group, "channel_id": env.channel_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(lifted.status(), 200);
    assert_eq!(chat(&env).await.0, 200, "解除后同进程立即放行");
    let after_lift = margin_breaker::evaluate(&env.pg, Some(&ch), &env.redis, now)
        .await
        .unwrap();
    assert!(!after_lift.tripped.iter().any(|t| t.group_code == env.group));
    let blocks = env.state.sched.margin_blocks().await.unwrap();
    let entry = &blocks[&okapi::margin::field(&env.group, env.channel_id)];
    assert_eq!(entry.state, okapi::margin::BlockState::Lifted);
    let audits = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs WHERE action = 'margin.lift' AND target = $1"#,
        okapi::margin::field(&env.group, env.channel_id)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(audits, 1);

    // 关闭功能：整表清空（其它并行用例不受本用例遗留影响）
    set_breaker(&env, json!({"enabled": false})).await;
    margin_breaker::evaluate(&env.pg, Some(&ch), &env.redis, now)
        .await
        .unwrap();
    assert!(env.state.sched.margin_blocks().await.unwrap().is_empty());
}
