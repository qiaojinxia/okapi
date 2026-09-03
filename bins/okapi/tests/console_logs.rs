//! 全站日志检索与实时 KPI 验收（IMPLEMENTATION §11.12）。
//!
//! 覆盖三件此前完全缺位的能力：`/admin/logs` 逐笔明细检索、
//! `/admin/logs/stat` 统计条的双数据源切换、`/admin/stats/realtime` 秒桶实时档。
//! 依赖 .env 与 ClickHouse（scripts/dev-deps.sh up）；未配 CH 时相关端点按约定 501。

use okapi::worker::chsink;
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

struct Env {
    pg: PgPool,
    state: gateway::state::AppState,
    addr: SocketAddr,
    super_token: String,
    user_token: String,
    user_id: i64,
    username: String,
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
    let model = format!("log-{}", &suffix[..12]);

    let super_id = okapi_store::provision::create_user(&pg, &format!("ls-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-log-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-log-s")
        .await
        .unwrap();

    let username = format!("lu-{suffix}");
    let user_id = okapi_store::provision::create_user(&pg, &username)
        .await
        .unwrap();
    let user_token = format!("sk-okapi-log-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-log-u")
        .await
        .unwrap();

    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let channel_name = format!("log-ch-{suffix}");
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
        model,
        channel_id,
        channel_name,
    }
}

/// 一笔明细的 outbox payload。`is_error` 由 log_type=5 表达（chsink 据此推导）。
fn payload(env: &Env, error_code: Option<&str>) -> Value {
    json!({
        "request_id": Uuid::new_v4(),
        "user_id": env.user_id,
        "api_key_id": 7,
        "group": "default",
        "model": env.model,
        "channel_id": env.channel_id,
        "channel_key_id": 1,
        "log_type": if error_code.is_some() { 5 } else { 2 },
        "prompt_tokens": 100,
        "cached_tokens": 40,
        "completion_tokens": 200,
        "reasoning_tokens": 0,
        "amount_micro": 1_000,
        "original_amount_micro": 1_250,
        "discount_micro": 250,
        "pricing_epoch": 1,
        "latency_ms": 900,
        "ttft_ms": 120,
        "is_stream": true,
        "sticky_layer": 2,
        "failover_count": 1,
        "upstream_status": if error_code.is_some() { 429 } else { 200 },
        "error_code": error_code.unwrap_or(""),
        "node": "test-node",
        "client_type": "test-cli",
    })
}

async fn seed(env: &Env, ok: usize, errors: &[&str]) {
    let mut rows: Vec<Value> = Vec::new();
    for _ in 0..ok {
        rows.push(payload(env, None));
    }
    for code in errors {
        rows.push(payload(env, Some(code)));
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

/// 轮询直到 `data` 出现命中行（outbox 是全局队列，同文件并行用例会抢锁，
/// 故必须 drain+查询重试而非 drain 一次就断言——与 console_stats 同法）。
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

/// 新端点都锁在 billing.read 后面。
#[tokio::test]
async fn log_endpoints_require_billing_read() {
    let env = setup().await;
    for path in [
        "/admin/logs",
        "/admin/logs/stat",
        "/admin/stats/realtime",
        "/admin/stats/errors",
        "/admin/stats/groups",
        "/admin/diagnose",
    ] {
        let (status, _) = get(&env, path, &env.user_token).await;
        assert_eq!(status, 403, "{path} 应拒绝无 billing.read 的用户");
    }
}

/// 死信队列控制面：列表带 payload 摘要；重投把 payload 送回 outbox 并删行；
/// 丢弃只改状态不删行（审计留痕）且不再计入未处理深度；RBAC 双门槛。
// 一条处置脚本：播种 → 列表 → 重投 → 丢弃 → 深度/可见性/幂等，拆开会割裂状态流转语义
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn dlq_list_requeue_and_discard() {
    let env = setup().await;
    let req_id = Uuid::new_v4();
    let mut ids = Vec::new();
    for (amount, err) in [
        (1_234_i64, "clickhouse: connection refused"),
        (1_235_i64, "invalid payload shape"),
    ] {
        let id = sqlx::query_scalar!(
            r#"INSERT INTO billing_dlq (source, payload, error, retry_count)
               VALUES ('chsink', $1, $2, 5) RETURNING id"#,
            json!({ "request_id": req_id, "user_id": env.user_id, "model": env.model,
                    "amount_micro": amount }),
            err
        )
        .fetch_one(&env.pg)
        .await
        .unwrap();
        ids.push(id);
    }

    // 权限：读要 billing.read，重投/丢弃要 billing.refund；普通用户两者皆 403
    let (status, _) = get(&env, "/admin/dlq", &env.user_token).await;
    assert_eq!(status, 403);
    let client = reqwest::Client::new();
    let post = |path: &str, token: &str, body: Value| {
        let url = format!("http://{}{path}", env.addr);
        let client = client.clone();
        let token = token.to_owned();
        async move {
            let resp = client
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap();
            (
                resp.status().as_u16(),
                resp.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    let (status, _) = post("/admin/dlq/requeue", &env.user_token, json!({ "ids": ids })).await;
    assert_eq!(status, 403);

    // 列表：两行都在待处理里，摘要字段齐备
    let (status, body) = get(&env, "/admin/dlq?limit=200", &env.super_token).await;
    assert_eq!(status, 200, "{body}");
    let pending_before = body["pending"].as_i64().unwrap();
    let mine: Vec<&Value> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["request_id"].as_str() == Some(&req_id.to_string()))
        .collect();
    assert_eq!(mine.len(), 2, "{body}");
    assert_eq!(mine[0]["user_id"], env.user_id);
    assert_eq!(mine[0]["model"], env.model);
    assert!(
        mine[0]["amount_micro"].is_i64(),
        "金额摘要应为整数：{}",
        mine[0]
    );

    // 重投第一条：outbox 出现 billing.completed 行、DLQ 行消失
    let (status, body) = post(
        "/admin/dlq/requeue",
        &env.super_token,
        json!({ "ids": [ids[0]] }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["requeued"], 1);
    let in_outbox = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_outbox
           WHERE topic = 'billing.completed' AND payload->>'request_id' = $1"#,
        req_id.to_string()
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(in_outbox, 1, "payload 应回到 outbox");

    // 丢弃第二条：行保留、status=2、resolved_by 记操作者
    let (status, body) = post(
        "/admin/dlq/discard",
        &env.super_token,
        json!({ "ids": [ids[1]] }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["discarded"], 1);
    let row = sqlx::query!(
        r#"SELECT status, resolved_by, resolved_at FROM billing_dlq WHERE id = $1"#,
        ids[1]
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(row.status, 2);
    assert!(
        row.resolved_by.is_some() && row.resolved_at.is_some(),
        "审计留痕"
    );

    // 深度只数未处理：两条都处理完，pending 回落 2；缺省列表不含已丢弃，all=true 含
    let (_, after) = get(&env, "/admin/dlq?limit=200", &env.super_token).await;
    assert_eq!(after["pending"].as_i64().unwrap(), pending_before - 2);
    assert!(
        !after["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"].as_i64() == Some(ids[1])),
        "缺省列表不含已丢弃"
    );
    let (_, all) = get(&env, "/admin/dlq?limit=200&all=true", &env.super_token).await;
    assert!(
        all["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"].as_i64() == Some(ids[1])),
        "all=true 应含已丢弃行供回看"
    );
    // 重复丢弃/重投已处理的行：幂等，计数 0
    let (_, again) = post(
        "/admin/dlq/requeue",
        &env.super_token,
        json!({ "ids": [ids[1]] }),
    )
    .await;
    assert_eq!(again["requeued"], 0, "已丢弃的行不可再重投");

    sqlx::query!("DELETE FROM billing_dlq WHERE id = ANY($1)", &ids)
        .execute(&env.pg)
        .await
        .unwrap();
}

/// 全链路健康有了 HTTP 出口：与 MCP diagnose 同一函数，字段齐备且不依赖 CH 在线。
#[tokio::test]
async fn diagnose_over_http_reports_components() {
    let env = setup().await;
    let (status, body) = get(&env, "/admin/diagnose", &env.super_token).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["postgres"], true);
    assert_eq!(body["redis"], true);
    // CH 配了就是 bool，没配是 null——前端据此区分"不可达"与"未启用"
    assert!(body["clickhouse"].is_boolean() || body["clickhouse"].is_null());
    assert!(body["outbox_pending"].as_i64().unwrap() >= 0);
    assert!(body["dlq_depth"].as_i64().unwrap() >= 0);
    assert!(body["cooling_keys"].as_i64().unwrap() >= 0);
    assert!(body["pricebook_epoch"].is_number());
}

/// 分组经营：mv_group_day 首个控制面出口，带分组倍率回填与收入占比。
#[tokio::test]
async fn group_stats_expose_revenue_share() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    // 播种行 group=default（payload 固定）；default 分组必然存在且倍率可回填
    seed(&env, 3, &["upstream_error"]).await;
    let row = poll_row(&env, "/admin/stats/groups?days=1", |r| {
        r["group"].as_str() == Some("default") && r["requests"].as_i64().unwrap_or(0) >= 4
    })
    .await;
    assert!(
        row["group_ratio"].is_string(),
        "分组倍率应从 PG 回填：{row}"
    );
    assert!(row["amount_micro"].as_i64().unwrap() >= 3_000);
    assert!(row["share_bp"].as_i64().unwrap() > 0);
    assert!(row["error_rate_bp"].as_i64().unwrap() > 0, "含一笔失败");
}

/// 明细检索：按模型过滤命中，且 id 列被回填为人看得懂的名字。
#[tokio::test]
async fn log_search_resolves_names_and_columns() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    seed(&env, 3, &[]).await;

    let path = format!("/admin/logs?model={}&hours=1", env.model);
    let row = poll_row(&env, &path, |r| {
        r["model"].as_str() == Some(env.model.as_str())
    })
    .await;

    assert_eq!(row["username"], env.username, "user_id 应回填用户名");
    assert_eq!(
        row["channel_name"], env.channel_name,
        "channel_id 应回填渠道名"
    );
    assert_eq!(row["provider"], "openai", "provider 取 PG 渠道行");
    assert_eq!(row["usage"]["prompt_tokens"], 100);
    assert_eq!(row["usage"]["cached_tokens"], 40);
    assert_eq!(row["amount_micro"], 1_000);
    assert_eq!(row["discount_micro"], 250);
    assert_eq!(row["ttft_ms"], 120, "TTFT 是排障必需列，MCP 的 PG 版查不到");
    assert_eq!(row["is_stream"], true);
    assert_eq!(row["client_type"], "test-cli");
    assert_eq!(row["is_error"], false);
}

/// 只看失败：errors_only 把成功行挡在外面，且错误码/上游状态原样带出。
#[tokio::test]
async fn log_search_errors_only_filter() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    seed(&env, 2, &["upstream_rate_limited"]).await;

    let path = format!("/admin/logs?model={}&hours=1&errors_only=true", env.model);
    let row = poll_row(&env, &path, |r| {
        r["model"].as_str() == Some(env.model.as_str())
    })
    .await;
    assert_eq!(row["error_code"], "upstream_rate_limited");
    assert_eq!(row["upstream_status"], 429);
    assert_eq!(row["is_error"], true);

    // 该过滤下不得混入成功行
    let (status, body) = get(&env, &path, &env.super_token).await;
    assert_eq!(status, 200);
    for r in body["data"].as_array().unwrap() {
        assert_eq!(r["is_error"], true, "errors_only 不应返回成功行：{r}");
    }
}

/// 绝对时间区间（对账"某一天的账"）：含现在的区间命中，纯过去的区间为空；
/// 非法时间不静默退化为全表扫，而是 fail-fast 报错。
#[tokio::test]
async fn log_search_absolute_range() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    seed(&env, 2, &[]).await;
    let now = chrono::Utc::now();
    let from = (now - chrono::Duration::hours(1)).to_rfc3339();
    let to = (now + chrono::Duration::hours(1)).to_rfc3339();

    let path = format!(
        "/admin/logs?model={}&from={}&to={}",
        env.model,
        urlenc(&from),
        urlenc(&to)
    );
    let row = poll_row(&env, &path, |r| {
        r["model"].as_str() == Some(env.model.as_str())
    })
    .await;
    assert_eq!(row["model"], env.model);

    // 纯过去的区间：同样的模型过滤，什么都不该有
    let past_from = (now - chrono::Duration::days(30)).to_rfc3339();
    let past_to = (now - chrono::Duration::days(29)).to_rfc3339();
    let (status, body) = get(
        &env,
        &format!(
            "/admin/logs?model={}&from={}&to={}",
            env.model,
            urlenc(&past_from),
            urlenc(&past_to)
        ),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["from"], past_from, "响应回显区间供前端核对");

    // 非法 from：400 而非静默回落相对窗口——要"8 月 30 日"却拿到"最近 24 小时"比报错糟得多
    let (status, body) = get(&env, "/admin/logs?from=yesterday", &env.super_token).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "bad_request");
    // 统计条同一套过滤：同样 400
    let (status, _) = get(&env, "/admin/logs/stat?from=yesterday", &env.super_token).await;
    assert_eq!(status, 400);
}

fn urlenc(s: &str) -> String {
    s.replace('+', "%2B").replace(':', "%3A")
}

/// 统计条：窗口累计走 CH；RPM/TPM 的数据源随「是否带过滤」切换
/// （docs/database.md §3.5 末条的定案，无过滤才有 Redis 秒桶可用）。
#[tokio::test]
async fn log_stat_switches_rate_source() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    seed(&env, 4, &["upstream_rate_limited"]).await;

    // 带 user_id 过滤 → 口径精确到本用例，且速率退化为 CH 60s 窗
    let path = format!("/admin/logs/stat?user_id={}&hours=1", env.user_id);
    for _ in 0..50 {
        drain(&env).await;
        let (status, body) = get(&env, &path, &env.super_token).await;
        assert_eq!(status, 200, "{body}");
        if body["requests"].as_i64() == Some(5) {
            assert_eq!(body["errors"], 1);
            assert_eq!(body["error_rate_bp"], 2_000, "1/5 = 2000 基点");
            assert_eq!(body["tokens"], 1_500, "5 × (100+200)");
            assert_eq!(body["amount_micro"], 5_000);
            assert_eq!(body["cached_tokens"], 200);
            assert_eq!(body["cache_hit_bp"], 4_000, "40 缓存 / 100 输入 = 40%");
            assert_eq!(body["rate_source"], "clickhouse", "带过滤时退化为 CH 窗口");
            let (_, unfiltered) = get(&env, "/admin/logs/stat?hours=1", &env.super_token).await;
            assert_eq!(
                unfiltered["rate_source"], "redis",
                "无过滤时 RPM/TPM 应取 Redis 秒桶"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("统计条轮询超时");
}

/// 实时档：结算经 `settle_write` 收口，故一笔消费 + 一笔错误即应出现在秒桶里。
///
/// 必须等过一整秒——`kpi_window` 刻意跳过当前这一秒（它还在累加中，
/// 读进来会让每次刷新都看到一个偏低的尾点）。这个断言把该语义钉死。
#[tokio::test]
async fn realtime_kpi_counts_settlements() {
    let env = setup().await;
    // 断言落在"本用例写入那几秒"的桶上，而不是整窗合计的差值：共享的开发 Redis 上
    // 其它用例/并行会话的流量在 60s 窗里滚进滚出，整窗差值会被滚出的旧秒抵消（实测
    // before=27 after=27，两笔确实写进去了、同时有两笔从窗尾掉出）。
    let started = chrono::Utc::now().timestamp();

    for (log_type, error_code) in [(2_i16, None), (5_i16, Some("upstream_error"))] {
        env.state
            .settle_write(okapi_ledger::SettlementInput {
                request_id: Uuid::new_v4(),
                log_type,
                user_id: env.user_id,
                api_key_id: 0,
                group_code: "default",
                model_name: &env.model,
                channel_id: None,
                channel_key_id: None,
                state: okapi_domain::BillingState::Committed,
                usage: okapi_domain::TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 20,
                    ..Default::default()
                },
                amount: Money::from_micros(1_000),
                original: Money::from_micros(1_000),
                discount: Money::ZERO,
                pricing_epoch: None,
                pricing_snapshot: None,
                latency_ms: 5,
                ttft_ms: None,
                is_stream: false,
                retry_count: 0,
                failover_count: 0,
                upstream_status: Some(200),
                error_code,
                upstream_request_id: None,
                node: "test-node",
                sticky_layer: 0,
                client_type: "test",
                client_ip: None,
                delta_micro: -1_000,
                balance_after: None,
                event_type: "commit",
            })
            .await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    let (status, body) = get(&env, "/admin/stats/realtime?window=60", &env.super_token).await;
    assert_eq!(status, 200, "实时档不依赖 CH，任何形态都应可用：{body}");
    let series = body["series"].as_array().unwrap();
    let since_start = |field: &str| -> i64 {
        series
            .iter()
            .filter(|p| p["ts"].as_i64().unwrap_or(0) >= started - 1)
            .map(|p| p[field].as_i64().unwrap_or(0))
            .sum()
    };
    assert!(
        since_start("requests") >= 2,
        "两笔结算应进入写入那几秒的桶：{body}"
    );
    assert!(since_start("errors") >= 1, "错误笔数应单独计数：{body}");
    assert!(body["requests"].as_i64().unwrap() >= 2);
    assert!(body["qps_milli"].as_i64().unwrap() > 0, "QPS 应有读数");
    assert_eq!(
        body["series"].as_array().map(Vec::len),
        Some(60),
        "序列应逐秒铺满窗口（供 sparkline）"
    );
    assert_eq!(body["window_secs"], 60);
}

/// 代客用量视图（CH 在线）：按日与按模型聚合出自 mv_key_model_day 用户前缀。
#[tokio::test]
async fn admin_user_usage_aggregates_from_ch() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    seed(&env, 3, &[]).await;
    let path = format!("/admin/users/{}/usage?days=1", env.user_id);
    for _ in 0..50 {
        drain(&env).await;
        let (status, body) = get(&env, &path, &env.super_token).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["stats_available"], true);
        let hit = body["by_model"].as_array().and_then(|m| {
            m.iter()
                .find(|r| r["model"].as_str() == Some(env.model.as_str()))
        });
        if let Some(row) = hit
            && row["requests"].as_i64() == Some(3)
        {
            assert_eq!(row["amount_micro"], 3_000);
            assert_eq!(row["tokens"], 900, "3 × (100+200)");
            let daily = body["daily"].as_array().unwrap();
            assert!(!daily.is_empty(), "按日序列应有今日桶");
            assert!(
                daily
                    .iter()
                    .any(|d| d["requests"].as_i64().unwrap_or(0) >= 3),
                "今日桶应含本用例三笔：{daily:?}"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("轮询超时：用户用量未聚合出本用例模型");
}

/// 资金流入概要：充值 / 兑换补偿 / 扣减 / 过期分列（PG-only，不依赖 CH）。
#[tokio::test]
async fn cashflow_separates_recharge_from_grants() {
    let env = setup().await;
    // 三类事件各一笔：真金充值 5$、兑换入账 2$、管理员扣减 1$
    for (event_type, delta, actor) in [
        ("recharge", 5_000_000_i64, "system:payment"),
        ("adjust", 2_000_000, "system:redeem"),
        ("adjust", -1_000_000, "admin:1"),
    ] {
        sqlx::query!(
            r#"INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, payload, actor)
               VALUES ($1, $2, $3, $4, '{}', $5)"#,
            env.user_id,
            Uuid::new_v4(),
            event_type,
            delta,
            actor
        )
        .execute(&env.pg)
        .await
        .unwrap();
    }

    let (status, body) = get(&env, "/admin/stats/cashflow?days=1", &env.super_token).await;
    assert_eq!(status, 200, "cashflow 不依赖 CH，应恒可用：{body}");
    let today = &body["today"];
    // 全站口径：断言下界而非精确值（并行用例可能同时入账）
    assert!(
        today["recharge_micro"].as_i64().unwrap() >= 5_000_000,
        "充值应计入 recharge：{today}"
    );
    assert!(
        today["granted_micro"].as_i64().unwrap() >= 2_000_000,
        "兑换入账应与充值分列：{today}"
    );
    assert!(
        today["clawed_micro"].as_i64().unwrap() >= 1_000_000,
        "负向 adjust 应记为扣减：{today}"
    );
    // 无权限 403
    let (denied, _) = get(&env, "/admin/stats/cashflow", &env.user_token).await;
    assert_eq!(denied, 403);
}

/// 错误分布：按错误码归并，并带出出问题最多的渠道——
/// 「错误率 3%」不可行动，「三成是这条渠道的 429」才可行动。
#[tokio::test]
async fn error_breakdown_groups_by_code() {
    let env = setup().await;
    if env.state.ch.is_none() {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    let code = format!("e2e_{}", &Uuid::new_v4().simple().to_string()[..8]);
    seed(&env, 1, &[&code, &code, &code]).await;

    let row = poll_row(&env, "/admin/stats/errors?days=1&limit=100", |r| {
        r["error_code"].as_str() == Some(code.as_str())
    })
    .await;
    assert_eq!(row["errors"], 3);
    assert_eq!(row["upstream_status"], 429);
    assert_eq!(row["top_channel_id"], env.channel_id);
    assert_eq!(row["top_channel_name"], env.channel_name);
    assert_eq!(row["top_model"], env.model);
    assert!(
        row["share_bp"].as_i64().unwrap() > 0,
        "占比应按整数基点给出"
    );
}
