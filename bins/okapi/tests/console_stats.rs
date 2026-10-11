//! 运营看板端点验收（IMPLEMENTATION §10）：渠道健康 / 模型时延分位 / 经营毛利。
//! 三者只读 CH 物化视图，此前这些列（errors、ttft_q、upstream_cost）无任何查询出口。
//! 依赖 .env 与 ClickHouse（scripts/dev-deps.sh up）；未配 CH 时端点按约定 501。

use futures::FutureExt as _;
use okapi::worker::chsink;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

#[path = "support/latency_statistics.rs"]
mod latency_statistics;

#[path = "support/ttft_average.rs"]
mod ttft_average;

#[path = "support/usage_sources.rs"]
mod usage_sources;

#[path = "support/cache_statistics.rs"]
mod cache_statistics;
#[path = "support/cache_ttl_statistics.rs"]
mod cache_ttl_statistics;
#[path = "support/historical_speech_units.rs"]
mod historical_speech_units;
#[path = "support/input_unit_statistics.rs"]
mod input_unit_statistics;
#[path = "support/statistics_sql_capture.rs"]
mod statistics_sql_capture;
#[path = "support/token_detail_aggregates.rs"]
mod token_detail_aggregates;

#[path = "support/ttft_statistics.rs"]
mod ttft_statistics;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[path = "support/output_rate_statistics.rs"]
mod output_rate_statistics;

#[path = "support/population_storage.rs"]
mod population_storage;

#[path = "support/quality_pagination.rs"]
mod quality_pagination;

#[path = "support/dashboard_calendar.rs"]
mod dashboard_calendar;

struct Env {
    pg: PgPool,
    state: gateway::state::AppState,
    addr: SocketAddr,
    super_token: String,
    user_token: String,
    user_id: i64,
    model: String,
    channel_id: i64,
    channel_name: String,
}

async fn setup() -> Env {
    // Deliberately incomplete legacy fixtures in other suites must not become this view's history.
    let database = std::env::var("OKAPI_TEST_CH_DATABASE")
        .unwrap_or_else(|_| format!("okapi_stats_{}", Uuid::new_v4().simple()));
    setup_with_ch_database(&database).await
}

async fn setup_with_ch_database(ch_database: &str) -> Env {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("stat-{}", &suffix[..12]);

    let super_id = okapi_store::provision::create_user(&pg, &format!("ss-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-stat-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-stat-s")
        .await
        .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("su-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-stat-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-stat-u")
        .await
        .unwrap();

    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let channel_name = format!("stat-ch-{suffix}");
    let (channel_id, _key_id) = okapi_store::provision::create_channel(
        &pg,
        &channel_name,
        "openai",
        "http://127.0.0.1:1/v1",
        "mock",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let mut state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    if let Some(url) = ch_url.as_deref() {
        state.ch = Some(okapi_store::ChClient::new(url, ch_database).unwrap());
    }
    if let Some(client) =
        statistics_sql_capture::configured(ch_url.as_deref(), Some(ch_database)).await
    {
        state.ch = Some(client);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = console::router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    Env {
        pg,
        state,
        addr,
        super_token,
        user_token,
        user_id,
        model,
        channel_id,
        channel_name,
    }
}

/// 造一笔明细的 outbox payload。
/// `is_error` 经 `log_type = 5` 表达：chsink 的 `is_error` 由 log_type 推导
/// （`build_ch_row`），payload 里没有独立的错误标志位。
fn payload(env: &Env, amount: i64, discount: i64, ttft: i64, is_error: bool) -> Value {
    json!({
        "request_id": Uuid::new_v4(),
        "user_id": env.user_id,
        "api_key_id": 1,
        "group": "default",
        "model": env.model,
        "channel_id": env.channel_id,
        "channel_key_id": 1,
        "log_type": if is_error { 5 } else { 2 },
        "prompt_source":"upstream",
        "completion_source":"upstream",
        "upstream_usage":{"prompt_tokens":100,"completion_tokens":200},
        "input_unit":"tokens",
        "input_characters":null,
        "prompt_tokens": 100,
        "cached_tokens": 0,
        "completion_tokens": 200,
        "reasoning_tokens": 0,
        "amount_micro": amount,
        "original_amount_micro": amount + discount,
        "discount_micro": discount,
        "pricing_epoch": 1,
        "latency_ms": 1_000,
        "ttft_ms": ttft,
        "is_stream": true,
        "sticky_layer": 2,
        "failover_count": 1,
        "error_code": if is_error { "upstream_error" } else { "" },
        "node": "test-node",
        "client_type": "test",
    })
}

/// 批量播明细：数量要压过库里其它用例的遗留流量，否则排行榜口径的端点
/// （`ORDER BY requests DESC LIMIT n`）会把本用例的渠道/模型挤出结果。
async fn seed_many(env: &Env, ok: usize, err: usize, ttft: i64) {
    let mut rows: Vec<Value> = Vec::with_capacity(ok + err);
    for i in 0..ok {
        // TTFT 递增以便分位数有分布
        rows.push(payload(
            env,
            1_000,
            250,
            ttft + i64::try_from(i).unwrap_or(0),
            false,
        ));
    }
    for _ in 0..err {
        rows.push(payload(env, 1_000, 250, ttft, true));
    }
    sqlx::query!(
        r#"INSERT INTO billing_outbox (topic, payload)
           SELECT 'request_log', p FROM UNNEST($1::jsonb[]) AS p"#,
        &rows
    )
    .execute(&env.pg)
    .await
    .unwrap();
}

async fn drain(env: &Env) {
    let Some(ch) = env.state.ch.as_ref() else {
        return;
    };
    ch.ensure_schema().await.unwrap();
    for _ in 0..100 {
        if chsink::process_once(&env.pg, ch).await.unwrap() == 0 {
            break;
        }
    }
}

/// drain + 查询直到谓词成立。
///
/// 光调 `drain` 是不够的：outbox 是全局队列，并行用例持锁时 `process_once` 会
/// 返回 0（没有可认领的行）而本用例的行其实还在排队，drain 于是提前收敛，紧接着
/// 的断言就读到少一行的结果。这在长期 dev 库上表现为随机红。
async fn poll_until<F>(env: &Env, path: &str, token: &str, ready: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    for _ in 0..50 {
        drain(env).await;
        let (status, body) = get(env, path, token).await;
        assert_eq!(status, 200, "{path} 应 200：{body}");
        if ready(&body) {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{path} 轮询超时");
}

async fn get(env: &Env, path: &str, token: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://{}{path}", env.addr))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

/// 个人年度活动覆盖闰日与年末，默认当前 key，全账户也不能越过 user 边界。
#[tokio::test]
async fn personal_activity_covers_calendar_year_and_isolates_owners() {
    let env = setup().await;
    let path = "/api/me/stats/activity?year=2024";
    assert_eq!(get(&env, path, "invalid-key").await.0, 401);
    let Some(ch) = env.state.ch.as_ref() else {
        assert_eq!(get(&env, path, &env.user_token).await.0, 501);
        return;
    };
    ch.ensure_schema().await.unwrap();
    let (_, me) = get(&env, "/api/me", &env.user_token).await;
    let key_id = me["key_id"].as_i64().unwrap();
    seed_activity_history(&env, ch, key_id).await;
    let (status, activity) = get(&env, path, &env.user_token).await;
    assert_eq!(status, 200, "{activity}");
    assert_eq!(activity["year"], 2024);
    assert_eq!(activity["first_year"], 2024);
    assert_eq!(activity["scope"], "key");
    assert!(activity["timezone"].as_str().is_some_and(|s| !s.is_empty()));
    let rows = activity["data"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "年末应包含，下年元旦应排除");
    let leap = rows.iter().find(|row| row["day"] == "2024-02-29").unwrap();
    assert_eq!(
        leap["requests"], 1,
        "默认 key 范围不能混入其他 key 或其他用户"
    );
    assert_eq!(leap["prompt_tokens"], 1000);
    assert_eq!(leap["completion_tokens"], 500);
    assert_eq!(leap["amount_micro"], 12500);
    let (status, account) = get(&env, &format!("{path}&scope=user"), &env.user_token).await;
    assert_eq!(status, 200);
    let leap = account["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["day"] == "2024-02-29")
        .unwrap();
    assert_eq!(leap["requests"], 2, "全账户应合并两个 key，仍排除其他用户");
    assert_eq!(
        get(&env, "/api/me/stats/activity?year=65535", &env.user_token)
            .await
            .0,
        400
    );
    assert_eq!(
        get(
            &env,
            "/api/me/stats/activity?scope=invalid",
            &env.user_token
        )
        .await
        .0,
        400
    );
    let (status, empty) = get(&env, "/api/me/stats/activity?year=2023", &env.user_token).await;
    assert_eq!(status, 200);
    assert!(empty["data"].as_array().unwrap().is_empty());
    // Classification loss after raw expires must not invent old call counts.
    ch.execute(&format!("ALTER TABLE population_v1_mv_key_model_day DELETE WHERE user_id={} SETTINGS mutations_sync=2", env.user_id)).await.unwrap();
    let (status, missing) = get(&env, path, &env.user_token).await;
    assert_eq!(status, 500, "{missing}");
    assert_eq!(
        missing["error"]["param"],
        "statistics_request_history_incomplete"
    );
}

/// Typed calls produce the retained aggregates; raw is removed before the read.
async fn seed_activity_history(env: &Env, ch: &okapi_store::ChClient, key_id: i64) {
    let mut rows = Vec::new();
    for (user, key, day) in [
        (env.user_id, key_id, "2024-02-28"),
        (env.user_id, key_id, "2024-02-29"),
        (env.user_id, key_id, "2024-12-31"),
        (env.user_id, key_id, "2025-01-01"),
        (env.user_id, 0, "2024-02-29"),
        (0, key_id, "2024-02-29"),
    ] {
        let mut row = payload(env, 12500, 0, 100, false);
        row["user_id"] = json!(user);
        row["api_key_id"] = json!(key);
        row["ts"] = json!(format!("{day} 12:00:00.000"));
        row["prompt_tokens"] = json!(1000);
        row["cached_tokens"] = json!(400);
        row["completion_tokens"] = json!(500);
        row["reasoning_tokens"] = json!(200);
        row["upstream_usage"] = json!({"prompt_tokens":1000,"completion_tokens":500});
        rows.push(chsink::js_payload_to_ch_row(&row));
    }
    ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
        .await
        .unwrap();
    ch.execute(&format!(
        "ALTER TABLE request_log_raw DELETE WHERE model='{}' SETTINGS mutations_sync=2",
        env.model
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn portal_charts_expose_cache_writes_performance_and_exact_date_window() {
    let env = setup().await;
    let Some(ch) = env.state.ch.as_ref() else {
        return;
    };
    ch.ensure_schema().await.unwrap();
    let (_, me) = get(&env, "/api/me", &env.user_token).await;
    let key_id = me["key_id"].as_i64().unwrap();
    for writes in [20, 0] {
        let mut row = payload(&env, 1000, 250, 100, false);
        row["api_key_id"] = json!(key_id);
        row["cache_write_tokens"] = json!(writes);
        row["cache_read_reported"] = json!(true);
        row["cache_write_reported"] = json!(true);
        sqlx::query("INSERT INTO billing_outbox (topic, payload) VALUES ('request_log', $1)")
            .bind(row)
            .execute(&env.pg)
            .await
            .unwrap();
    }
    // 必须轮询到两行都可见再断言：drain 一次就查，并行用例持着 outbox 锁时本用例的行
    // 还在排队（见 poll_until 的注释）。此处此前漏了迁移——这条用例是全量跑里最常见的
    // "随机红"，在变异测试里三次被记成与被测代码无关的假抓手。
    let path = "/api/me/stats/breakdown?days=7";
    let report = poll_until(&env, path, &env.user_token, |b| b["total"]["requests"] == 2).await;
    assert_eq!(report["days"], 7);
    let start = chrono::NaiveDate::parse_from_str(
        report["window"]["start_date"].as_str().unwrap(),
        "%Y-%m-%d",
    )
    .unwrap();
    let end = chrono::NaiveDate::parse_from_str(
        report["window"]["end_date"].as_str().unwrap(),
        "%Y-%m-%d",
    )
    .unwrap();
    assert_eq!((end - start).num_days(), 6, "7 天含首尾，不应变成 8 天");
    assert_eq!(report["total"]["cache_write_tokens"], 20);
    assert_eq!(report["total"]["tokens"], 600, "缓存写入仍含在输入中");
    assert_eq!(report["total"]["original_micro"], 2500);
    assert_eq!(report["total"]["avg_latency_ms"], 1000);
    assert_eq!(report["total"]["avg_ttft_ms"], 100);
    assert_eq!(report["total"]["ttft_samples"], 2);
    assert_eq!(report["total"]["cache_read_known_requests"], 2);
    assert_eq!(report["total"]["cache_hit_bp"], 0, "明确上报的零命中保留 0");
    assert_eq!(report["total"]["tokens_per_1k_sec"], 200_000);
    let today = report["window"]["today"].as_str().unwrap();
    let (status, one_day) = get(
        &env,
        &format!("{path}&start_date={today}&end_date={today}"),
        &env.user_token,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(one_day["days"], 1);
    assert_eq!(one_day["total"]["requests"], 2);
    for range in [
        "start_date=2024-03-01&end_date=2024-02-29",
        "start_date=2023-02-29&end_date=2024-02-29",
        "start_date=2024-01-01",
        "start_date=2020-01-01&end_date=2024-01-01",
    ] {
        assert_eq!(
            get(&env, &format!("{path}&{range}"), &env.user_token)
                .await
                .0,
            400
        );
    }
    // 旧事件缺此字段：已知零与未采集不能合并成同一种显示。
    let mut legacy = payload(&env, 1000, 250, 100, false);
    legacy["api_key_id"] = json!(key_id);
    sqlx::query("INSERT INTO billing_outbox (topic, payload) VALUES ('request_log', $1)")
        .bind(legacy)
        .execute(&env.pg)
        .await
        .unwrap();
    let report = poll_until(&env, path, &env.user_token, |b| b["total"]["requests"] == 3).await;
    assert!(report["total"]["cache_write_tokens"].is_null());
    assert_eq!(report["total"]["cache_write_known_requests"], 2);
    assert!(
        report["total"]["cache_hit_bp"].is_null(),
        "未上报不等于零命中"
    );
    assert_eq!(report["total"]["cache_read_known_requests"], 2);
}

#[tokio::test]
async fn portal_identity_matches_web_session_owner_without_exposing_key_secret() {
    let env = setup().await;
    let (_, me) = get(&env, "/api/me", &env.user_token).await;
    assert_eq!(me["has_web_session"], false);
    assert_eq!(me["key_prefix"], "sk-stat-u");
    assert!(me["key_name"].is_string());
    assert!(!me.to_string().contains(&env.user_token));
    let sid = okapi::gateway::sched_redis::SchedulerRedis::web_session_sid(
        env.user_id,
        &Uuid::new_v4().simple().to_string(),
    );
    env.state
        .sched
        .web_session_set(&sid, env.user_id, None, None)
        .await;
    for (token, expected) in [(&env.user_token, true), (&env.super_token, false)] {
        let response: Value = reqwest::Client::new()
            .get(format!("http://{}/api/me", env.addr))
            .bearer_auth(token)
            .header("Cookie", format!("okapi_session={sid}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["has_web_session"], expected);
    }
}

/// 轮询直到 `data` 里出现命中行。
///
/// outbox 是全局队列且 `process_once` 用 `FOR UPDATE SKIP LOCKED`——同文件的并行
/// 用例持锁时本用例的 drain 循环会提前收敛到 0，此刻自己播的行尚未进 CH。
/// 故必须"drain + 查询"重试，而非 drain 一次就断言。
async fn poll_row<F>(env: &Env, path: &str, matches: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    for _ in 0..50 {
        drain(env).await;
        let (status, body) = get(env, path, &env.super_token).await;
        assert_eq!(status, 200, "{path} 应 200：{body}");
        if let Some(hit) = body["data"]
            .as_array()
            .and_then(|d| d.iter().find(|r| matches(r)))
        {
            return hit.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{path} 轮询超时：未等到命中行");
}

/// 无 billing.read 权限一律 403（三个端点同一守卫）。
#[tokio::test]
async fn stats_require_billing_read() {
    let env = setup().await;
    for path in [
        "/admin/stats/channels",
        "/admin/stats/models",
        "/admin/stats/margin",
    ] {
        let (status, _) = get(&env, path, &env.user_token).await;
        assert_eq!(status, 403, "{path} 应拒绝无权限用户");
    }
}

/// 渠道健康：错误率按基点、TTFT 分位、粘性与切换计数都出得来。
#[tokio::test]
async fn channel_health_exposes_error_rate_and_ttft() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    // 60 成功 + 20 失败 → 错误率 2500bp（25%）
    seed_many(&env, 60, 20, 100).await;

    let row = poll_row(&env, "/admin/stats/channels?days=1&limit=100", |r| {
        r["channel_id"].as_i64() == Some(env.channel_id) && r["requests"].as_i64() == Some(80)
    })
    .await;
    assert_eq!(row["requests"], 80);
    assert_eq!(row["errors"], 20);
    assert_eq!(row["error_rate_bp"], 2500, "20/80 = 2500 基点");
    assert!(row["ttft_p50_ms"].as_i64().unwrap() > 0, "TTFT 分位应有值");
    assert_eq!(row["failovers"], 80, "每笔 failover_count=1");
    assert_eq!(row["sticky_hits"], 80, "每笔 sticky_layer=2");
    assert_eq!(row["sticky_rate_bp"], 10_000);
    // 200 completion tokens / 1000ms = 200 tok/s → 放大千倍
    assert_eq!(row["tokens_per_1k_sec"], 200_000);
    assert_eq!(row["name"], env.channel_name, "渠道名应由 PG 补齐");
}

/// 模型分位：mv_model_hour 此前完全无出口，这里钉死 TTFT/latency 两组分位都出来。
#[tokio::test]
async fn model_latency_quantiles_exposed() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    seed_many(&env, 80, 0, 120).await;

    let row = poll_row(&env, "/admin/stats/models?days=1&limit=100", |r| {
        r["model"].as_str() == Some(env.model.as_str()) && r["requests"].as_i64() == Some(80)
    })
    .await;
    assert_eq!(row["requests"], 80);
    // TTFT 120..199 递增 → p50 落在区间内，p99 不低于 p50
    let p50 = row["ttft_p50_ms"].as_i64().unwrap();
    let p99 = row["ttft_p99_ms"].as_i64().unwrap();
    assert!((120..200).contains(&p50), "p50 应落在播种区间：{p50}");
    assert!(p99 >= p50, "p99 不得低于 p50");
    assert_eq!(row["latency_p50_ms"], 1_000);
    assert_eq!(row["tokens_per_1k_sec"], 200_000);
}

/// 在本用例独占的 CH 库中验证真实物化视图、排序、时间粒度与折叠守恒。
/// 不向共享库写入大额“排名锚点”，也不消费其他用例的 outbox。
#[tokio::test]
async fn model_trend_folds_tail_into_other() {
    let database = format!("okapi_trend_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let Some(ch) = env.state.ch.as_ref() else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    };
    ch.ensure_schema().await.unwrap();
    // 即使断言 panic，也先删掉本测试创建的唯一数据库，再重新抛出失败。
    let result = std::panic::AssertUnwindSafe(check_model_trend(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_model_trend(env: &Env, ch: &okapi_store::ChClient) {
    let path = "/admin/stats/model-trend?days=1&limit=20";
    let (status, empty) = get(env, path, &env.super_token).await;
    assert_eq!(status, 200, "{empty}");
    assert_eq!(empty["models"], json!([]));
    assert_eq!(empty["data"], json!([]));

    let now = chrono::Utc::now();
    let mut rows = Vec::new();
    // 总额故意不按字典序；同额尾部由模型名稳定排序。跨两个小时验证逐桶折叠。
    for (model, amount, hours_ago) in [
        ("z-largest", 300, 2),
        ("z-largest", 100, 1),
        ("a-second", 200, 2),
        ("tail-z", 10, 2),
        ("tail-a", 10, 1),
    ] {
        let mut row = payload(env, amount, 0, 100, false);
        row["model"] = json!(model);
        row["ts"] = json!(
            (now - chrono::Duration::hours(hours_ago))
                .format("%Y-%m-%d %H:%M:%S%.3f")
                .to_string()
        );
        rows.push(chsink::js_payload_to_ch_row(&row));
    }
    ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
        .await
        .unwrap();

    let (status, wide) = get(env, path, &env.super_token).await;
    assert_eq!(status, 200, "{wide}");
    assert_eq!(wide["granularity"], "hour");
    assert_eq!(
        wide["models"],
        json!(["z-largest", "a-second", "tail-a", "tail-z"])
    );
    let buckets = wide["data"].as_array().unwrap();
    assert_eq!(buckets.len(), 2);
    assert!(buckets[0]["bucket"].as_str() < buckets[1]["bucket"].as_str());

    for (days, limit, granularity) in [(1, 1, "hour"), (1, 0, "hour"), (7, 1, "day")] {
        let (status, narrow) = get(
            env,
            &format!("/admin/stats/model-trend?days={days}&limit={limit}"),
            &env.super_token,
        )
        .await;
        assert_eq!(status, 200, "{narrow}");
        assert_eq!(narrow["granularity"], granularity);
        assert_eq!(narrow["models"], json!(["z-largest", "__other"]));
        let series_total = |series: &str, metric: &str| -> i64 {
            narrow["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|bucket| bucket["values"][series][metric].as_i64().unwrap_or(0))
                .sum()
        };
        assert_eq!(series_total("z-largest", "amount_micro"), 400);
        assert_eq!(series_total("__other", "amount_micro"), 220);
        assert_eq!(series_total("z-largest", "requests"), 2);
        assert_eq!(series_total("__other", "requests"), 3);
        if days == 1 {
            let folded = narrow["data"].as_array().unwrap();
            assert_eq!(folded.len(), buckets.len());
            for (original, compact) in buckets.iter().zip(folded) {
                assert_eq!(original["bucket"], compact["bucket"]);
                assert_eq!(
                    original["values"]["z-largest"],
                    compact["values"]["z-largest"]
                );
                for metric in ["amount_micro", "requests"] {
                    let tail: i64 = original["values"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(model, _)| model.as_str() != "z-largest")
                        .map(|(_, value)| value[metric].as_i64().unwrap())
                        .sum();
                    assert_eq!(compact["values"]["__other"][metric], tail);
                }
            }
        }
    }
}

/// 客户端类型分布（#5277）：按 client_type 归并，去重用户数与错误率齐备。
///
/// client_type 取唯一值（不像模型趋势有 Top N 挤出问题——这里 limit=100
/// 而站点上真实的客户端种类只有十几种，不会被挤出）。
#[tokio::test]
async fn client_distribution_groups_by_client_type() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    let client = format!("cli-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let mut rows: Vec<Value> = Vec::new();
    for i in 0..5 {
        let mut p = payload(&env, 1_000, 0, 100, i == 4);
        p["client_type"] = json!(client);
        rows.push(p);
    }
    sqlx::query!(
        r#"INSERT INTO billing_outbox (topic, payload)
           SELECT 'request_log', p FROM UNNEST($1::jsonb[]) AS p"#,
        &rows
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let row = poll_row(&env, "/admin/stats/clients?days=1&limit=100", |r| {
        r["client_type"].as_str() == Some(client.as_str()) && r["requests"].as_i64() == Some(5)
    })
    .await;
    assert_eq!(row["errors"], 1);
    assert_eq!(row["error_rate_bp"], 2_000, "1/5 = 2000 基点");
    assert_eq!(row["users"], 1, "同一用户播的 5 笔 → 去重后 1 人");
    assert_eq!(row["tokens"], 1_500, "5 × (100+200)");
    assert_eq!(row["amount_micro"], 5_000);
    // 占比是整数基点：在长期 dev 库上本用例的 5 笔可能不足万分之一（截断为 0），
    // 所以不断言 > 0，只断言它是合法基点，且全表各行之和不超过 10000（截断只会少不会多）
    let share = row["share_bp"].as_i64().expect("share_bp 应为整数基点");
    assert!((0..=10_000).contains(&share), "占比基点越界：{share}");
    let (_, all) = get(
        &env,
        "/admin/stats/clients?days=1&limit=100",
        &env.super_token,
    )
    .await;
    let sum: i64 = all["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["share_bp"].as_i64().unwrap_or(0))
        .sum();
    assert!(sum <= 10_000, "各行占比之和不得超过 10000：{sum}");
}

/// 经营口径：实收 / 标价 / 让利逐日聚合，毛利 = 实收 − 上游成本。全整数。
///
/// 注：`upstream_cost_micro` 目前全链路恒为 0（chsink `build_ch_row` 硬编码，
/// 且渠道侧只有相对成本系数 `relative_cost_milli` 供调度用，推不出绝对成本），
/// 故本用例只断言实收与让利；毛利字段在成本采集落地后自然生效。
#[tokio::test]
async fn margin_report_sums_amount_and_discount() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    seed_many(&env, 4, 0, 100).await;

    // margin 是全站口径，本用例的行必然被淹没，故先查本用户日聚合核对精确值
    let ch = env.state.ch.as_ref().unwrap();
    for _ in 0..50 {
        drain(&env).await;
        let probe = ch
            .query_json_each_row(&format!(
                "SELECT countMerge(requests) AS r FROM mv_user_day WHERE user_id = {} GROUP BY user_id",
                env.user_id
            ))
            .await
            .unwrap();
        if !probe.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (status, body) = get(&env, "/admin/stats/margin?days=1", &env.super_token).await;
    assert_eq!(status, 200, "{body}");
    let mine = ch
        .query_json_each_row(&format!(
            "SELECT sumMerge(amount) AS a, sumMerge(upstream_cost) AS c, \
                    sumMerge(discount) AS d FROM mv_user_day \
             WHERE user_id = {} GROUP BY user_id",
            env.user_id
        ))
        .await
        .unwrap();
    let cell = |key: &str| -> i64 {
        mine.first().map_or(0, |r| {
            r.get(key).map_or(0, |v| {
                v.as_str()
                    .map_or_else(|| v.as_i64(), |s| s.parse().ok())
                    .unwrap_or(0)
            })
        })
    };
    assert_eq!(cell("a"), 4_000, "实收 = 4 × 1000");
    assert_eq!(cell("d"), 1_000, "让利 = 4 × 250");

    // 端点侧：总计字段齐备且毛利恒等于 实收 − 成本
    let total = &body["total"];
    let amount = total["amount_micro"].as_i64().unwrap();
    assert!(total["cost_known_requests"].as_i64().unwrap() < total["requests"].as_i64().unwrap());
    assert!(
        total["margin_micro"].is_null(),
        "legacy costs cannot imply total profit"
    );
    if let Some(margin) = total["known_margin_micro"].as_i64() {
        assert_eq!(
            margin,
            total["known_amount_micro"].as_i64().unwrap()
                - total["known_cost_micro"].as_i64().unwrap()
        );
    }
    assert!(amount >= 4_000, "全站实收应含本用例两笔");
    assert!(total["discount_micro"].as_i64().unwrap() >= 1_000);
    assert!(total["error_rate_bp"].is_i64());
    let (status, overview) = get(&env, "/admin/stats/overview?days=1", &env.super_token).await;
    assert_eq!(status, 200, "{overview}");
    assert!(overview["today"]["margin_micro"].is_null());
    assert_eq!(
        overview["today"]["requests"], overview["window"]["requests"],
        "one calendar day includes today only"
    );
    // 逐日明细含标价列（账单解释器同源口径）
    assert!(
        body["data"].as_array().is_some_and(|d| d
            .iter()
            .any(|r| r["original_micro"].as_i64().unwrap_or(0) > 0)),
        "逐日明细应含标价"
    );
}
