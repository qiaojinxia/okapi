//! 运营看板端点验收（IMPLEMENTATION §10）：渠道健康 / 模型时延分位 / 经营毛利。
//! 三者只读 CH 物化视图，此前这些列（errors、ttft_q、upstream_cost）无任何查询出口。
//! 依赖 .env 与 ClickHouse（scripts/dev-deps.sh up）；未配 CH 时端点按约定 501。

use okapi::worker::chsink;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

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
    dotenvy::dotenv().ok();
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
    assert_eq!(status, 200);
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
    let cost = total["upstream_cost_micro"].as_i64().unwrap();
    assert_eq!(total["margin_micro"].as_i64().unwrap(), amount - cost);
    assert!(amount >= 4_000, "全站实收应含本用例两笔");
    assert!(total["discount_micro"].as_i64().unwrap() >= 1_000);
    assert!(total["error_rate_bp"].is_i64());
    // 逐日明细含标价列（账单解释器同源口径）
    assert!(
        body["data"].as_array().is_some_and(|d| d
            .iter()
            .any(|r| r["original_micro"].as_i64().unwrap_or(0) > 0)),
        "逐日明细应含标价"
    );
}
