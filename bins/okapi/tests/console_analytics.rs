//! 用量分析端点验收（IMPLEMENTATION §11.13）：mv_cube_hour 三端点（趋势 / 拆分 / 流向），
//! 以及站点规模（PG）、列表行内用量、单渠道时间线。
//! 依赖 .env 与 ClickHouse（scripts/dev-deps.sh up）；未配 CH 时立方体端点按约定 501。

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
    username: String,
    key_id: i64,
    model_a: String,
    model_b: String,
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
    // 模型名取固定前缀 + 随机后缀：立方体端点全部带 user_id 过滤，不会与其它用例混流
    let model_a = format!("cube-a-{}", &suffix[..10]);
    let model_b = format!("cube-b-{}", &suffix[..10]);

    let super_id = okapi_store::provision::create_user(&pg, &format!("cs-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-cube-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-cube-s")
        .await
        .unwrap();

    let username = format!("cu-{suffix}");
    let user_id = okapi_store::provision::create_user(&pg, &username)
        .await
        .unwrap();
    let user_token = format!("sk-okapi-cube-u-{suffix}");
    let key_id =
        okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-cube-u")
            .await
            .unwrap();
    sqlx::query!(
        "UPDATE api_keys SET name = 'cube-key' WHERE id = $1",
        key_id
    )
    .execute(&pg)
    .await
    .unwrap();

    for m in [&model_a, &model_b] {
        okapi_store::provision::create_model_ratio(&pg, m, "1", "1", "1")
            .await
            .unwrap();
    }
    let channel_name = format!("cube-ch-{suffix}");
    let (channel_id, _key) = okapi_store::provision::create_channel(
        &pg,
        &channel_name,
        "openai",
        "http://127.0.0.1:1/v1",
        "mock",
        &[model_a.as_str(), model_b.as_str()],
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
        username,
        key_id,
        model_a,
        model_b,
        channel_id,
        channel_name,
    }
}

fn payload(env: &Env, model: &str, amount: i64, is_error: bool) -> Value {
    json!({
        "request_id": Uuid::new_v4(),
        "user_id": env.user_id,
        "api_key_id": env.key_id,
        "group": "default",
        "model": model,
        "channel_id": env.channel_id,
        "channel_key_id": 1,
        "log_type": if is_error { 5 } else { 2 },
        "prompt_tokens": 100,
        "cached_tokens": 40,
        "completion_tokens": 200,
        "reasoning_tokens": 10,
        "amount_micro": amount,
        "original_amount_micro": amount + 250,
        "discount_micro": 250,
        "pricing_epoch": 1,
        "latency_ms": 1_000,
        "ttft_ms": 120,
        "is_stream": true,
        "sticky_layer": 2,
        "failover_count": 0,
        "error_code": if is_error { "upstream_error" } else { "" },
        "node": "test-node",
        "client_type": "test",
    })
}

async fn seed(env: &Env, model: &str, ok: usize, err: usize) {
    let mut rows: Vec<Value> = Vec::with_capacity(ok + err);
    for _ in 0..ok {
        rows.push(payload(env, model, 1_000, false));
    }
    for _ in 0..err {
        rows.push(payload(env, model, 0, true));
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

/// drain + 查询直到谓词成立（outbox 是全局队列，并行用例持锁时 drain 会提前收敛）。
async fn poll_until<F>(env: &Env, path: &str, ready: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    for _ in 0..50 {
        drain(env).await;
        let (status, body) = get(env, path, &env.super_token).await;
        assert_eq!(status, 200, "{path} 应 200：{body}");
        if ready(&body) {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{path} 轮询超时");
}

fn has_ch(env: &Env) -> bool {
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return false;
    }
    true
}

/// 六个端点同一守卫：无 billing.read 一律 403。
#[tokio::test]
async fn analytics_require_billing_read() {
    let env = setup().await;
    for path in [
        "/admin/stats/trend",
        "/admin/stats/breakdown",
        "/admin/stats/flow",
        "/admin/stats/inventory",
        "/admin/stats/entity-usage?kind=user&ids=1",
        "/admin/stats/channels/1/timeline",
    ] {
        let (status, _) = get(&env, path, &env.user_token).await;
        assert_eq!(status, 403, "{path} 应拒绝无权限用户");
    }
}

/// 趋势：user_id 过滤只见本用户；model 过滤只见该模型；scope 回填名字；
/// 单日窗口按小时出桶；环比窗口为空。
#[tokio::test]
async fn trend_filters_by_dimension_and_resolves_scope() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    seed(&env, &env.model_a, 6, 2).await;
    seed(&env, &env.model_b, 4, 0).await;

    let path = format!("/admin/stats/trend?days=1&user_id={}", env.user_id);
    let body = poll_until(&env, &path, |b| b["total"]["requests"].as_i64() == Some(12)).await;
    assert_eq!(body["granularity"], "hour");
    assert_eq!(body["total"]["errors"], 2);
    assert_eq!(body["total"]["error_rate_bp"], 1_666, "2/12");
    assert_eq!(body["total"]["amount_micro"], 10_000, "10 笔成功 × 1000");
    assert_eq!(body["total"]["discount_micro"], 3_000, "12 笔 × 250");
    assert_eq!(body["total"]["prompt_tokens"], 1_200);
    assert_eq!(body["total"]["cached_tokens"], 480);
    assert_eq!(body["total"]["cache_hit_bp"], 4_000, "480/1200");
    assert_eq!(body["total"]["avg_latency_ms"], 1_000);
    assert_eq!(body["total"]["avg_ttft_ms"], 120);
    assert_eq!(body["scope"]["user"]["username"], env.username);
    assert!(body["previous"]["requests"].is_null() || body["previous"]["requests"] == 0);
    let buckets = body["data"].as_array().unwrap();
    assert!(!buckets.is_empty());
    assert!(
        buckets[0]["bucket"].as_str().unwrap().contains(':'),
        "小时桶应带时分秒：{}",
        buckets[0]["bucket"]
    );

    // 叠加 model 过滤
    let path = format!(
        "/admin/stats/trend?days=1&user_id={}&model={}",
        env.user_id, env.model_b
    );
    let (status, body) = get(&env, &path, &env.super_token).await;
    assert_eq!(status, 200);
    assert_eq!(body["total"]["requests"], 4);
    assert_eq!(body["scope"]["model"], env.model_b);

    // 多日窗口按天出桶
    let path = format!("/admin/stats/trend?days=7&user_id={}", env.user_id);
    let (_, body) = get(&env, &path, &env.super_token).await;
    assert_eq!(body["granularity"], "day");
    assert!(!body["data"][0]["bucket"].as_str().unwrap().contains(':'));

    // 按模型堆叠：两条序列，逐桶求和守恒；limit=1 时 B 折进 __other
    let path = format!(
        "/admin/stats/trend?days=1&user_id={}&stack=model",
        env.user_id
    );
    let (_, body) = get(&env, &path, &env.super_token).await;
    assert_eq!(body["stack"], "model");
    let series: Vec<&str> = body["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        series,
        vec![env.model_a.as_str(), env.model_b.as_str()],
        "按金额降序"
    );
    let sum: i64 = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| b["values"].as_object().unwrap().values())
        .map(|v| v["requests"].as_i64().unwrap())
        .sum();
    assert_eq!(sum, 12);
    let path = format!(
        "/admin/stats/trend?days=1&user_id={}&stack=model&limit=1",
        env.user_id
    );
    let (_, body) = get(&env, &path, &env.super_token).await;
    assert_eq!(body["series"][1]["key"], "__other");
    assert!(body["series"][1]["label"].is_null());

    // 按渠道堆叠：序列标签从 PG 回填渠道名
    let path = format!(
        "/admin/stats/trend?days=1&user_id={}&stack=channel",
        env.user_id
    );
    let (_, body) = get(&env, &path, &env.super_token).await;
    assert_eq!(body["series"][0]["label"], env.channel_name);
}

/// 拆分：按模型给占比 / 名次；按渠道回填名字与 provider；按 provider 折叠；
/// 按 key 回填名字 + 属主；非法维度 400。
#[tokio::test]
async fn breakdown_by_each_dimension() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    seed(&env, &env.model_a, 6, 0).await;
    seed(&env, &env.model_b, 2, 0).await;

    let base = format!("&days=1&user_id={}", env.user_id);
    let body = poll_until(
        &env,
        &format!("/admin/stats/breakdown?by=model{base}"),
        |b| b["total_requests"].as_i64() == Some(8),
    )
    .await;
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["key"], env.model_a);
    assert_eq!(rows[0]["rank"], 1);
    assert_eq!(rows[0]["share_bp"], 7_500, "6000/8000");
    assert_eq!(rows[0]["request_share_bp"], 7_500);
    assert!(rows[0]["previous_rank"].is_null(), "上期无数据");
    assert!(rows[0]["delta_bp"].is_null(), "上期为 0 → 环比无意义");
    assert_eq!(rows[1]["key"], env.model_b);
    assert_eq!(rows[1]["share_bp"], 2_500);
    assert_eq!(body["total_amount_micro"], 8_000);

    let (_, body) = get(
        &env,
        &format!("/admin/stats/breakdown?by=channel{base}"),
        &env.super_token,
    )
    .await;
    let row = &body["data"][0];
    assert_eq!(row["channel_id"], env.channel_id);
    assert_eq!(row["label"], env.channel_name);
    assert_eq!(row["provider"], "openai");
    assert_eq!(row["requests"], 8);

    let (_, body) = get(
        &env,
        &format!("/admin/stats/breakdown?by=provider{base}"),
        &env.super_token,
    )
    .await;
    let row = &body["data"][0];
    assert_eq!(row["key"], "openai");
    assert_eq!(row["channels"], 1);
    assert_eq!(row["amount_micro"], 8_000);
    assert_eq!(row["share_bp"], 10_000);

    let (_, body) = get(
        &env,
        &format!("/admin/stats/breakdown?by=api_key{base}"),
        &env.super_token,
    )
    .await;
    let row = &body["data"][0];
    assert_eq!(row["api_key_id"], env.key_id);
    assert_eq!(row["label"], "cube-key");
    assert_eq!(row["user_id"], env.user_id);
    assert_eq!(row["username"], env.username);

    let (_, body) = get(
        &env,
        &format!("/admin/stats/breakdown?by=user{base}"),
        &env.super_token,
    )
    .await;
    assert_eq!(body["data"][0]["label"], env.username);

    let (_, body) = get(
        &env,
        &format!("/admin/stats/breakdown?by=group{base}"),
        &env.super_token,
    )
    .await;
    assert_eq!(body["data"][0]["key"], "default");
    assert!(
        body["data"][0]["group_ratio"].is_string(),
        "分组倍率应由 PG 回填"
    );

    let (status, _) = get(
        &env,
        &format!("/admin/stats/breakdown?by=planet{base}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 400);
}

/// 流向：五阶段节点齐全、相邻链接守恒、限定单用户时覆盖率 100%、
/// limit=1 时次要模型折进 `__other`。
#[tokio::test]
async fn flow_links_conserve_and_fold_other() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    seed(&env, &env.model_a, 6, 0).await;
    seed(&env, &env.model_b, 2, 0).await;

    let path = format!(
        "/admin/stats/flow?days=1&metric=requests&limit=2&user_id={}",
        env.user_id
    );
    let body = poll_until(&env, &path, |b| b["total"].as_i64() == Some(8)).await;
    assert_eq!(body["coverage_bp"], 10_000);
    assert_eq!(body["truncated"], false);
    let nodes = body["nodes"].as_array().unwrap();
    let find = |id: &str| nodes.iter().find(|n| n["id"] == id).cloned();
    let user_node = find(&format!("user:{}", env.user_id)).expect("用户节点");
    assert_eq!(user_node["value"], 8);
    assert_eq!(user_node["label"], env.username);
    let key_node = find(&format!("api_key:{}", env.key_id)).expect("key 节点");
    assert_eq!(key_node["label"], "cube-key");
    assert_eq!(find(&format!("model:{}", env.model_a)).unwrap()["value"], 6);
    assert_eq!(find(&format!("model:{}", env.model_b)).unwrap()["value"], 2);
    let ch_node = find(&format!("channel:{}", env.channel_id)).expect("渠道节点");
    assert_eq!(ch_node["label"], env.channel_name);

    // 每一跳的链接值之和 = 总量（守恒）
    let links = body["links"].as_array().unwrap();
    for (from, to) in [
        ("user:", "node:"),
        ("node:", "api_key:"),
        ("api_key:", "group:"),
        ("group:", "model:"),
        ("model:", "channel:"),
    ] {
        let sum: i64 = links
            .iter()
            .filter(|l| {
                l["source"].as_str().unwrap().starts_with(from)
                    && l["target"].as_str().unwrap().starts_with(to)
            })
            .map(|l| l["value"].as_i64().unwrap())
            .sum();
        assert_eq!(sum, 8, "{from}→{to} 链接应守恒");
    }

    // limit=1：模型阶段只留 Top1，次要模型折进 __other
    let path = format!(
        "/admin/stats/flow?days=1&metric=requests&limit=1&user_id={}",
        env.user_id
    );
    let (_, body) = get(&env, &path, &env.super_token).await;
    let nodes = body["nodes"].as_array().unwrap();
    let other = nodes
        .iter()
        .find(|n| n["id"] == "model:__other")
        .expect("应有其他桶");
    assert_eq!(other["value"], 2);
    assert_eq!(other["other"], true);
    assert!(other["label"].is_null());

    let (status, _) = get(&env, "/admin/stats/flow?metric=bananas", &env.super_token).await;
    assert_eq!(status, 400);
}

/// 站点规模：纯 PG，不依赖 CH；渠道健康按"有可用 key"口径。
#[tokio::test]
async fn inventory_counts_without_clickhouse() {
    let env = setup().await;
    let (status, body) = get(&env, "/admin/stats/inventory", &env.super_token).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["users"]["total"].as_i64().unwrap() >= 2);
    assert!(
        body["users"]["new_today"].as_i64().unwrap() >= 2,
        "本用例刚建两个用户"
    );
    assert!(body["api_keys"]["active"].as_i64().unwrap() >= 2);
    assert!(body["channels"]["total"].as_i64().unwrap() >= 1);
    let healthy_before = body["channels"]["healthy"].as_i64().unwrap();
    assert!(healthy_before >= 1, "本用例渠道有一把可用 key");
    assert!(body["models"]["priced"].as_i64().unwrap() >= 2);
    assert!(
        body["models"]["served"].as_i64().unwrap() >= 2,
        "两模型都挂在启用渠道上"
    );
    assert!(body["channel_keys"]["active"].as_i64().unwrap() >= 1);
    assert!(body["groups"].as_i64().unwrap() >= 1);

    // 把本渠道的 key 全部置冷却 → 渠道从 healthy 移到 no_key
    sqlx::query!(
        "UPDATE channel_keys SET status = 2 WHERE channel_id = $1",
        env.channel_id
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let (_, after) = get(&env, "/admin/stats/inventory", &env.super_token).await;
    assert_eq!(
        after["channels"]["healthy"].as_i64().unwrap(),
        healthy_before - 1
    );
    assert!(after["channels"]["no_key"].as_i64().unwrap() >= 1);

    // 孤儿渠道（不在任何池）单独计数：对谁都不可达，落地页要能看见
    let orphan_before = after["channels"]["orphan"].as_i64().unwrap();
    okapi_store::admin::set_channel_pool_codes(&env.pg, env.channel_id, &[])
        .await
        .unwrap();
    let (_, after) = get(&env, "/admin/stats/inventory", &env.super_token).await;
    assert_eq!(
        after["channels"]["orphan"].as_i64().unwrap(),
        orphan_before + 1
    );
}

/// 行内用量：按用户 / 按 key 批量取今日与窗口消费；不存在的 id 不出现；非法 kind 400。
#[tokio::test]
async fn entity_usage_batches_by_ids() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    seed(&env, &env.model_a, 5, 1).await;

    let uid = env.user_id.to_string();
    let path = format!("/admin/stats/entity-usage?kind=user&ids={uid},999999999&days=7");
    let body = poll_until(&env, &path, |b| {
        b["data"][&uid]["requests"].as_i64() == Some(6)
    })
    .await;
    let row = &body["data"][&uid];
    assert_eq!(row["today_micro"], 5_000);
    assert_eq!(row["window_micro"], 5_000);
    assert!(row["last_day"].is_string());
    assert!(body["data"]["999999999"].is_null(), "无数据的 id 不出现");

    let kid = env.key_id.to_string();
    let path = format!("/admin/stats/entity-usage?kind=api_key&ids={kid}");
    let body = poll_until(&env, &path, |b| {
        b["data"][&kid]["requests"].as_i64() == Some(6)
    })
    .await;
    assert_eq!(body["data"][&kid]["window_micro"], 5_000);

    let (status, _) = get(
        &env,
        "/admin/stats/entity-usage?kind=planet&ids=1",
        &env.super_token,
    )
    .await;
    assert_eq!(status, 400);

    let (status, body) = get(
        &env,
        "/admin/stats/entity-usage?kind=user&ids=",
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["data"], json!({}));
}

/// 单渠道时间线：5 分钟桶带错误率与 TTFT 分位，汇总与播种一致。
#[tokio::test]
async fn channel_timeline_buckets() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    seed(&env, &env.model_a, 8, 2).await;

    let path = format!("/admin/stats/channels/{}/timeline?hours=1", env.channel_id);
    let body = poll_until(&env, &path, |b| b["requests"].as_i64() == Some(10)).await;
    assert_eq!(body["errors"], 2);
    assert_eq!(body["error_rate_bp"], 2_000);
    assert_eq!(body["channel_id"], env.channel_id);
    let data = body["data"].as_array().unwrap();
    assert!(!data.is_empty());
    let total: i64 = data.iter().map(|d| d["requests"].as_i64().unwrap()).sum();
    assert_eq!(total, 10);
    assert!(data[0]["bucket"].as_str().unwrap().contains(':'));
    assert!(data.iter().any(|d| d["ttft_p50_ms"].as_i64().unwrap() > 0));

    // hours 超界被 clamp 而非报错
    let (status, body) = get(
        &env,
        &format!(
            "/admin/stats/channels/{}/timeline?hours=9999",
            env.channel_id
        ),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["hours"], 168);
}

async fn insert_values(env: &Env, rows: &[Value], day: Option<&str>) {
    for row in rows {
        sqlx::query("INSERT INTO billing_outbox (topic, payload, created_at) VALUES ('request_log', $1, COALESCE($2::text::timestamptz, now()))")
            .bind(row).bind(day).execute(&env.pg).await.unwrap();
    }
}
fn advanced_payload(
    env: &Env,
    upstream: &str,
    group: &str,
    latency: i64,
    known: bool,
    cost: i64,
) -> Value {
    let mut row = payload(env, &env.model_a, 1000, false);
    row["requested_model"] = json!("client.alias");
    row["upstream_model"] = json!(upstream);
    row["endpoint"] = json!("/v1/responses");
    row["upstream_endpoint"] = json!("/v1/chat/completions");
    row["billing_type"] = json!("ratio");
    row["group"] = json!(group);
    row["latency_ms"] = json!(latency);
    row["ttft_ms"] = json!(latency / 10);
    row["upstream_cost_known"] = json!(known);
    row["upstream_cost_micro"] = json!(cost);
    row["cache_write_tokens"] = json!(0);
    row
}

#[tokio::test]
async fn advanced_filters_calendar_quality_and_partial_cost_are_consistent() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    let rows = vec![
        advanced_payload(&env, "up.a", "g,a", 1000, true, 0),
        advanced_payload(&env, "up.a", "g,a", 3000, true, 500),
        advanced_payload(&env, "up.b", "g'b", 8000, false, 0),
    ];
    insert_values(&env, &rows, Some("2026-08-25 12:00:00+00")).await;
    insert_values(&env, &[rows[0].clone()], Some("2026-08-24 12:00:00+00")).await;
    let path = format!(
        "/admin/stats/trend?start_date=2026-08-25&end_date=2026-08-25&granularity=hour&user_id={}",
        env.user_id
    );
    let body = poll_until(&env, &path, |b| b["total"]["requests"] == 3).await;
    assert_eq!(body["previous"]["requests"], 1);
    assert_eq!(
        body["total"]["cost_known_requests"], 2,
        "known zero cost is covered"
    );
    assert_eq!(body["total"]["cost_coverage_bp"], 6666);
    assert_eq!(body["total"]["known_margin_micro"], 1500);
    assert!(
        body["total"]["margin_micro"].is_null(),
        "partial coverage cannot be total margin"
    );
    assert_eq!(body["total"]["cache_write_tokens"], 0);
    assert_eq!(body["total"]["avg_latency_ms"], 4000);
    assert!(body["window"]["freshness"]["last_ingested_at"].is_string());
    let mut query_url = reqwest::Url::parse("http://localhost/").unwrap();
    let mut query = query_url.query_pairs_mut();
    query.extend_pairs([
        ("start_date", "2026-08-25"),
        ("end_date", "2026-08-25"),
        ("model_source", "upstream"),
        ("models", "[\"up.a\"]"),
        ("groups", "[\"g,a\"]"),
        ("endpoint", "/v1/responses"),
        ("upstream_endpoint", "/v1/chat/completions"),
        ("request_type", "stream"),
        ("billing_type", "ratio"),
        ("node", "test-node"),
    ]);
    query.append_pair("user_id", &env.user_id.to_string());
    drop(query);
    let query = query_url.query().unwrap();
    let (status, trend) = get(
        &env,
        &format!("/admin/stats/trend?{query}&stack=model_group&metric=latency"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{trend}");
    assert_eq!(trend["total"]["requests"], 2);
    assert_eq!(trend["total"]["margin_micro"], 1500);
    assert_eq!(trend["series"][0]["key"], "[\"up.a\",\"g,a\"]");
    let value = trend["data"][0]["values"]["[\"up.a\",\"g,a\"]"].clone();
    assert_eq!(value["avg_latency_ms"], 2000);
    assert_eq!(value["avg_ttft_ms"], 200);
    assert_eq!(value["avg_output_tps_milli"], 100000);
    let (status, by) = get(
        &env,
        &format!("/admin/stats/breakdown?{query}&by=model"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{by}");
    assert_eq!(by["total_requests"], 2);
    assert_eq!(by["data"][0]["key"], "up.a");
    assert_eq!(by["data"][0]["known_margin_micro"], 1500);
    let stages = "stages=%5B%22node%22,%22model%22,%22channel%22%5D";
    let (status, flow) = get(
        &env,
        &format!("/admin/stats/flow?{query}&metric=requests&{stages}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{flow}");
    assert_eq!(flow["total"], 2);
    assert_eq!(flow["stages"], json!(["node", "model", "channel"]));
    assert!(
        flow["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "model:up.a")
    );
    assert!(
        flow["links"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["value"] == 2)
    );
    let (status, none) = get(
        &env,
        &format!("/admin/stats/trend?{query}&stream=false"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(none["total"]["requests"], 0);
}

#[tokio::test]
async fn legacy_aggregate_remainder_is_preserved_once_and_marked_unknown() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    insert_values(
        &env,
        &[advanced_payload(&env, "up.a", "default", 1000, true, 0)],
        Some("2026-08-26 12:00:00+00"),
    )
    .await;
    drain(&env).await;
    let ch = env.state.ch.as_ref().unwrap();
    // Simulate pre-upgrade aggregate history sharing the exact old cube key with one new record.
    ch.execute(&format!("INSERT INTO mv_cube_hour SELECT toDateTime('2026-08-26 12:00:00') AS hour, toInt64({}) AS user_id, toInt64({}) AS api_key_id, 'default' AS group_code, '{}' AS model, toInt64({}) AS channel_id, countState() AS requests, sumState(toUInt64(100)) AS prompt_tokens, sumState(toUInt64(40)) AS cached_tokens, sumState(toUInt64(200)) AS completion_tokens, sumState(toUInt64(10)) AS reasoning_tokens, sumState(toInt64(1000)) AS amount, sumState(toInt64(250)) AS discount, sumState(toInt64(0)) AS upstream_cost, sumState(toUInt64(0)) AS errors, sumState(toUInt64(5000)) AS latency_sum, sumState(toUInt64(500)) AS ttft_sum, countIfState(toUInt32(500)>0) AS ttft_samples FROM numbers(2) GROUP BY hour, user_id, api_key_id, group_code, model, channel_id", env.user_id, env.key_id, env.model_a, env.channel_id)).await.unwrap();
    let query = format!(
        "start_date=2026-08-26&end_date=2026-08-26&user_id={}",
        env.user_id
    );
    let (status, trend) = get(
        &env,
        &format!("/admin/stats/trend?{query}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{trend}");
    assert_eq!(trend["total"]["requests"], 3);
    assert_eq!(trend["total"]["amount_micro"], 3000);
    assert_eq!(trend["total"]["cost_known_requests"], 1);
    assert_eq!(trend["total"]["avg_latency_ms"], 3666);
    assert!(trend["total"]["cache_write_tokens"].is_null());
    let (_, by) = get(
        &env,
        &format!("/admin/stats/breakdown?{query}&by=endpoint"),
        &env.super_token,
    )
    .await;
    let rows = by["data"].as_array().unwrap();
    assert_eq!(rows.iter().find(|r| r["key"] == "").unwrap()["requests"], 2);
    assert_eq!(
        rows.iter().find(|r| r["key"] == "/v1/responses").unwrap()["requests"],
        1
    );
}

#[tokio::test]
async fn invalid_analysis_ranges_and_dimensions_are_rejected() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    drain(&env).await;
    for query in [
        "start_date=2026-08-01",
        "start_date=2026-08-30&end_date=2026-08-01",
        "start_date=2026-02-30&end_date=2026-03-01",
        "start_date=2099-01-01&end_date=2099-01-02",
        "start_date=2025-01-01&end_date=2026-08-01",
        "start_date=2026-06-01&end_date=2026-08-01&granularity=hour",
        "model_source=invalid",
        "request_type=invalid",
        "models=not-json",
        "groups=%5B%22%22%5D",
    ] {
        let (status, body) = get(
            &env,
            &format!("/admin/stats/trend?{query}"),
            &env.super_token,
        )
        .await;
        assert_eq!(status, 400, "{query}: {body}");
    }
    for stages in [
        "%5B%22node%22%5D",
        "%5B%22node%22,%22node%22%5D",
        "%5B%22node%22,%22oops%22%5D",
    ] {
        let (status, body) = get(
            &env,
            &format!("/admin/stats/flow?stages={stages}"),
            &env.super_token,
        )
        .await;
        assert_eq!(status, 400, "{body}");
    }
}

#[tokio::test]
async fn flow_names_include_safe_context_deleted_and_missing_identities() {
    let env = setup().await;
    if !has_ch(&env) {
        return;
    }
    sqlx::query("UPDATE api_keys SET name = '' WHERE id = $1")
        .bind(env.key_id)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET deleted_at = now() WHERE id = $1")
        .bind(env.channel_id)
        .execute(&env.pg)
        .await
        .unwrap();
    let missing = 3_000_000_000_i64 + env.user_id;
    let mut orphan = payload(&env, &env.model_a, 500, false);
    orphan["user_id"] = json!(missing);
    orphan["api_key_id"] = json!(missing);
    orphan["channel_id"] = json!(missing);
    insert_values(
        &env,
        &[payload(&env, &env.model_a, 1000, false), orphan],
        None,
    )
    .await;
    let path = format!(
        "/admin/stats/flow?days=1&model={}&metric=requests",
        env.model_a
    );
    let body = poll_until(&env, &path, |b| b["total"] == 2).await;
    let nodes = body["nodes"].as_array().unwrap();
    let node = |stage: &str, id: i64| {
        nodes
            .iter()
            .find(|n| n["id"] == format!("{stage}:{id}"))
            .unwrap()
    };
    assert_eq!(node("user", env.user_id)["label"], env.username);
    assert_eq!(node("api_key", env.key_id)["label"], "");
    assert_eq!(node("api_key", env.key_id)["entity_status"], "active");
    assert_eq!(node("api_key", env.key_id)["owner_name"], env.username);
    assert_eq!(node("api_key", env.key_id)["key_prefix"], "sk-cube-u");
    assert!(node("api_key", env.key_id).get("key_hash").is_none());
    assert_eq!(node("channel", env.channel_id)["label"], env.channel_name);
    assert_eq!(node("channel", env.channel_id)["provider"], "openai");
    assert_eq!(node("channel", env.channel_id)["entity_status"], "deleted");
    for stage in ["user", "api_key", "channel"] {
        assert!(node(stage, missing)["label"].is_null());
        assert_eq!(node(stage, missing)["entity_status"], "missing");
    }
}
