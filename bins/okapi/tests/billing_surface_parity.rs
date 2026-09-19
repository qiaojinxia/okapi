//! 跨出口对账：一笔真实请求的金额，必须在**每一个**读出口报同一个数。
//!
//! 现有套件是按出口切的——`console_logs` 验日志、`console_stats` 验统计、
//! `console_portal` 验门户、`pg_settlement` 验落库，每个都自己造数据自己断言。
//! 于是"同一笔账在两个出口对不上"这类缺陷谁都看不见：各自的用例都是绿的。
//!
//! 这里反过来走：打一笔请求，把 `billing_records.amount_micro` 当作**唯一权威**，
//! 然后要求其余出口逐个等于它。被钉住的是五个**互相独立的写侧累加器**
//! （它们不是同一份数据的不同视图，是五处各写各的）：
//!
//! | 累加器 | 写入点 | 谁读它 |
//! | --- | --- | --- |
//! | `billing_records.amount_micro` | PG 结算事务 | `/api/me/logs` |
//! | `billing_events.delta_micro` | PG 结算事务 | 对账重放、账户流水 |
//! | `users.balance_micro` | PG 结算事务（快照列） | 门户余额 |
//!
//! 注：`ledger.credit` 只动 Redis 热账本（PG 事件由调用方另记），所以本用例的
//! 种子充值不进 `users.balance_micro`。该列断言走**增量**而非绝对值——绝对值取决于
//! 这个用户此前有没有经过 PG 侧入账，不是本用例该管的。
//! | Redis 热余额 | ledger Lua commit | 预扣判定、`/v1/dashboard/billing/*` |
//! | `api_keys.used_micro` | PG 结算事务 | `/v1/dashboard/billing/usage`（生态口径） |
//!
//! 外加 ClickHouse 侧两个读出口（`/admin/logs`、`/admin/stats/overview`），
//! 它们经 outbox → chsink 异步落，是最容易和 PG 漂移的一段。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。CH 未配置时相关断言自跳过。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::worker::chsink;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

/// 上游报的 usage 固定，好让"应扣多少"只取决于价簿而非随机量。
const UP_PROMPT: i64 = 1000;
const UP_COMPLETION: i64 = 200;
/// 初始充值。
const CREDIT_MICRO: i64 = 50_000_000;

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    axum::Json(json!({
        "id": "cmpl", "object": "chat.completion", "model": req["model"],
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}}],
        "usage": {"prompt_tokens": UP_PROMPT, "completion_tokens": UP_COMPLETION}
    }))
    .into_response()
}

async fn mock_embeddings(_body: axum::body::Bytes) -> axum::response::Response {
    axum::Json(json!({
        "object": "list",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
        "usage": {"prompt_tokens": UP_PROMPT, "total_tokens": UP_PROMPT}
    }))
    .into_response()
}

async fn mock_rerank(_body: axum::body::Bytes) -> axum::response::Response {
    axum::Json(json!({
        "results": [{"index": 0, "relevance_score": 0.9}],
        "usage": {"prompt_tokens": UP_PROMPT, "total_tokens": UP_PROMPT}
    }))
    .into_response()
}

async fn mock_images(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    let n = usize::try_from(req["n"].as_u64().unwrap_or(1)).unwrap_or(1);
    axum::Json(json!({
        "created": 1_700_000_000,
        "data": (0..n).map(|_| json!({"url": "https://img.example/x.png"})).collect::<Vec<_>>()
    }))
    .into_response()
}

async fn mock_speech(_body: axum::body::Bytes) -> axum::response::Response {
    (
        [(axum::http::header::CONTENT_TYPE, "audio/mpeg")],
        vec![0xFFu8, 0xFB, 0x90, 0x00],
    )
        .into_response()
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    state: gateway::state::AppState,
    user_id: i64,
    key_id: i64,
    token: String,
    model: String,
    emb_model: String,
    tts_model: String,
    img_model: String,
    gateway: SocketAddr,
    console: SocketAddr,
    admin_token: String,
}

/// 种四个模型 + 一条挂着它们的渠道。
///
/// 每个计费端点一个模型，定价形态刻意不同（ratio 与 per_call 各有），
/// 好让对账断言同时压到两条算价路径。
async fn seed_models(
    pg: &PgPool,
    suffix: &str,
    mock: SocketAddr,
) -> (String, String, String, String) {
    let model = format!("parity-m-{suffix}");
    okapi_store::provision::create_model_ratio(pg, &model, "1.25", "4.0", "1.0")
        .await
        .unwrap();
    // 每个计费端点自带一个模型：定价形态不同（ratio / per_call），但对账口径一样
    let emb_model = format!("parity-e-{suffix}");
    okapi_store::provision::create_model_ratio(pg, &emb_model, "0.5", "1.0", "1.0")
        .await
        .unwrap();
    let tts_model = format!("parity-t-{suffix}");
    okapi_store::provision::create_model_ratio(pg, &tts_model, "2.0", "1.0", "1.0")
        .await
        .unwrap();
    let img_model = format!("parity-i-{suffix}");
    let img_id = okapi_store::provision::create_model_ratio(pg, &img_model, "1", "1", "1")
        .await
        .unwrap();
    // 图片按次计费 $0.04/张
    sqlx::query!(
        r#"UPDATE model_pricing SET pricing_mode = 'per_call', per_call_price_micro = 40000
           WHERE model_id = $1"#,
        img_id
    )
    .execute(pg)
    .await
    .unwrap();
    okapi_store::provision::create_channel(
        pg,
        &format!("parity-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[
            model.as_str(),
            emb_model.as_str(),
            tts_model.as_str(),
            img_model.as_str(),
        ],
        true,
        None,
    )
    .await
    .unwrap();

    (model, emb_model, tts_model, img_model)
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let mock = serve(
        Router::new()
            .route("/v1/chat/completions", post(mock_ok))
            .route("/v1/embeddings", post(mock_embeddings))
            .route("/v1/rerank", post(mock_rerank))
            .route("/v1/images/generations", post(mock_images))
            .route("/v1/audio/speech", post(mock_speech)),
    )
    .await;
    let (model, emb_model, tts_model, img_model) = seed_models(&pg, &suffix, mock).await;

    let user_id = okapi_store::provision::create_user(&pg, &format!("parity-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-parity-{suffix}");
    let key_id = okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-parity")
        .await
        .unwrap();

    let admin_id = okapi_store::provision::create_user(&pg, &format!("parity-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-parity-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-padm")
        .await
        .unwrap();

    // CH 必须带上：`/api/me/usage`、`/api/me/stats/daily`、`/admin/logs` 都由它支撑，
    // 传 None 会让这些出口回 501 stats_disabled，跨出口对账就只剩 PG 半边。
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let state = gateway::build_state(
        &database_url,
        &redis_url,
        "parity-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(CREDIT_MICRO))
        .await
        .unwrap();
    let gateway_addr = serve(gateway::router(state.clone())).await;
    let console_addr = serve(console::router(state.clone())).await;

    Bed {
        pg,
        state,
        user_id,
        key_id,
        token,
        model,
        emb_model,
        tts_model,
        img_model,
        gateway: gateway_addr,
        console: console_addr,
        admin_token,
    }
}

async fn chat(bed: &Bed) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&json!({"model": bed.model, "stream": false,
                      "messages": [{"role": "user", "content": "hello"}]}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn get(addr: SocketAddr, path: &str, token: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json::<Value>().await.unwrap_or(Value::Null))
}

/// 复刻 `dashboard::micro_to_usd_json(used × 100)` 的字面量。
///
/// 全程整数、比字符串：`total_usage` 是 JSON number，用 f64 读回来比较既踩计费红线
/// （禁浮点），也会被 `to_string` 的舍入摆一道。serde_json 开了 `arbitrary_precision`，
/// 数字保持字面形态，逐字符比才是准确的。
fn usd_literal(amount_micro: i64) -> Value {
    let cents = amount_micro.saturating_mul(100) / 10_000;
    let literal = format!("{}.{:02}", cents / 100, (cents % 100).abs());
    serde_json::from_str(&literal).expect("按整数拼出来的小数一定是合法 JSON number")
}

async fn snapshot_micro(pg: &PgPool, user_id: i64) -> i64 {
    sqlx::query_scalar!(r#"SELECT balance_micro FROM users WHERE id = $1"#, user_id)
        .fetch_one(pg)
        .await
        .unwrap()
}

/// 权威数：PG 结算行。等到 committed 为止。
async fn wait_record(pg: &PgPool, user_id: i64) -> (Uuid, i64, i64, i64) {
    for _ in 0..80 {
        if let Some(r) = sqlx::query!(
            r#"SELECT request_id, amount_micro, prompt_tokens, completion_tokens
               FROM billing_records WHERE user_id = $1 AND status = 20"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap()
        {
            return (
                r.request_id,
                r.amount_micro,
                i64::from(r.prompt_tokens),
                i64::from(r.completion_tokens),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("8s 内没等到 committed 结算行");
}

/// drain outbox → CH，再按谓词轮询。
///
/// outbox 是全局队列且 `process_once` 用 `FOR UPDATE SKIP LOCKED`，并行用例可能
/// 抢先 drain 掉本用例的行、也可能让本轮 drain 提前收敛；故谓词必须覆盖全部待
/// 断言字段（验证清单第 3 节第 5 条）。
async fn poll_ch<F>(bed: &Bed, path: &str, token: &str, ready: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    for _ in 0..80 {
        if let Some(ch) = bed.state.ch.as_ref() {
            ch.ensure_schema().await.unwrap();
            for _ in 0..50 {
                if chsink::process_once(&bed.pg, ch).await.unwrap() == 0 {
                    break;
                }
            }
        }
        let (status, body) = get(bed.console, path, token).await;
        assert_eq!(status, 200, "{path} 应 200：{body}");
        if ready(&body) {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{path} 轮询超时");
}

/// 一笔请求的金额，在九个出口必须是同一个数。
#[tokio::test]
async fn one_request_reports_the_same_amount_on_every_surface() {
    let bed = setup().await;
    let snapshot_before = snapshot_micro(&bed.pg, bed.user_id).await;
    assert_eq!(chat(&bed).await, 200);

    // —— 权威：PG 结算行 ——
    let (request_id, amount, prompt, completion) = wait_record(&bed.pg, bed.user_id).await;
    assert!(amount > 0, "这笔必须真扣到钱，否则后面全是 0 == 0 的假绿");
    assert_eq!(prompt, UP_PROMPT, "落库 token 应取上游报的值");
    assert_eq!(completion, UP_COMPLETION);

    // —— 写侧累加器 1：billing_events 的 commit ——
    let commit_delta = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(delta_micro), 0)::bigint AS "d!" FROM billing_events
           WHERE request_id = $1 AND event_type = 'commit'"#,
        request_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(
        commit_delta, -amount,
        "事件流的扣减必须等于结算行金额的负值"
    );

    // —— 写侧累加器 2：users.balance_micro 快照列（断言增量，见模块头）——
    assert_eq!(
        snapshot_micro(&bed.pg, bed.user_id).await - snapshot_before,
        -amount,
        "users 快照列的扣减与结算金额对不上"
    );

    // —— 写侧累加器 3：Redis 热余额 ——
    let hot = bed
        .state
        .ledger
        .balance(bed.user_id)
        .await
        .unwrap()
        .as_micros();
    assert_eq!(hot, CREDIT_MICRO - amount, "Redis 热余额与 PG 快照漂移");

    // —— 写侧累加器 4：api_keys.used_micro（生态口径端点的数据源）——
    let used = sqlx::query_scalar!(
        r#"SELECT used_micro FROM api_keys WHERE id = $1"#,
        bed.key_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(used, amount, "令牌用量累加器与结算金额对不上");

    // —— 读出口 1：门户日志 ——
    let (status, logs) = get(bed.console, "/api/me/logs", &bed.token).await;
    assert_eq!(status, 200, "{logs}");
    let row = logs["data"]
        .as_array()
        .expect("门户日志应回数组")
        .iter()
        .find(|r| r["request_id"] == json!(request_id.to_string()))
        .unwrap_or_else(|| panic!("门户日志里找不到本次请求：{logs}"));
    assert_eq!(row["amount_micro"], json!(amount), "门户日志金额不一致");
    // token 在门户日志里挂在 usage 子对象下（与数据面 usage 形状一致）
    assert_eq!(row["usage"]["prompt_tokens"], json!(prompt));
    assert_eq!(row["usage"]["completion_tokens"], json!(completion));

    // —— 读出口 3：new-api 生态兼容口径（美分）——
    let (status, dash) = get(bed.gateway, "/v1/dashboard/billing/usage", &bed.token).await;
    assert_eq!(status, 200, "{dash}");
    assert_eq!(
        dash["total_usage"],
        usd_literal(amount),
        "生态兼容口径与结算金额不一致：{dash}"
    );

    // —— 读出口 4 / 5：ClickHouse 侧（异步落，最容易与 PG 漂移）——
    if bed.state.ch.is_none() {
        eprintln!("跳过 CH 断言：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    // —— 读出口 4：门户用量汇总（CH 支撑，需先把 outbox 灌进去）——
    let usage = poll_ch(&bed, "/api/me/usage?days=1", &bed.token, |b| {
        b["total_amount_micro"] == json!(amount)
    })
    .await;
    assert_eq!(
        usage["total_amount_micro"],
        json!(amount),
        "门户用量汇总与结算金额不一致：{usage}"
    );

    let rid = request_id.to_string();
    let admin_logs = poll_ch(
        &bed,
        &format!("/admin/logs?hours=1&request_id={rid}"),
        &bed.admin_token,
        |b| {
            b["data"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["request_id"] == json!(rid)))
        },
    )
    .await;
    let ch_row = admin_logs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request_id"] == json!(rid))
        .unwrap();
    assert_eq!(
        ch_row["amount_micro"],
        json!(amount),
        "ClickHouse 日志金额与 PG 漂移：{ch_row}"
    );
    // 管理日志的 token 同样在 usage 子对象下
    assert_eq!(ch_row["usage"]["prompt_tokens"], json!(prompt));
    assert_eq!(ch_row["usage"]["completion_tokens"], json!(completion));
}

/// 退款后，九个出口必须同步回到"没花过钱"的状态。
///
/// 单独一例：退款走的是另一条写路径（`admin_refund`），此前只有 `pg_settlement`
/// 从库里验过，没人验过读出口跟不跟得上——门户余额回了、日志还挂着扣款，
/// 或者反过来，都是用户直接能看见的账不平。
#[tokio::test]
async fn refund_restores_every_surface() {
    let bed = setup().await;
    let snapshot_before = snapshot_micro(&bed.pg, bed.user_id).await;
    assert_eq!(chat(&bed).await, 200);
    let (request_id, amount, _, _) = wait_record(&bed.pg, bed.user_id).await;
    assert!(amount > 0);

    let refund = reqwest::Client::new()
        .post(format!("http://{}/admin/billing/refund", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"request_id": request_id, "reason": "surface parity"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refund.status().as_u16(), 200, "退款应成功");
    let refunded: Value = refund.json().await.unwrap();
    assert_eq!(
        refunded["outcome"], "refunded",
        "首次退款应是 refunded 而非幂等分支：{refunded}"
    );

    // 钱回到三处累加器
    assert_eq!(
        snapshot_micro(&bed.pg, bed.user_id).await,
        snapshot_before,
        "退款后 users 快照应回到请求前的值"
    );
    let hot = bed
        .state
        .ledger
        .balance(bed.user_id)
        .await
        .unwrap()
        .as_micros();
    assert_eq!(hot, CREDIT_MICRO, "退款后 Redis 热余额应回到充值额");
    let used = sqlx::query_scalar!(
        r#"SELECT used_micro FROM api_keys WHERE id = $1"#,
        bed.key_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(used, 0, "退款后令牌用量累加器应回冲到 0");

    // 结算行标记为已退款，且退款事件与扣款事件净额为 0
    let status = sqlx::query_scalar!(
        r#"SELECT status FROM billing_records WHERE request_id = $1"#,
        request_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(status, 30, "结算行应标记为已退款（30）");
    let net = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(delta_micro), 0)::bigint AS "d!" FROM billing_events
           WHERE request_id = $1"#,
        request_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(net, 0, "扣款 + 退款的事件净额必须为 0");

    // 生态口径也要回零
    let (code, dash) = get(bed.gateway, "/v1/dashboard/billing/usage", &bed.token).await;
    assert_eq!(code, 200, "{dash}");
    assert_eq!(
        dash["total_usage"],
        usd_literal(0),
        "退款后生态口径仍报用量：{dash}"
    );
}

/// 按 request_id 取权威结算行（表驱动用例里一次打多个端点，不能再按 user_id 取唯一行）。
async fn wait_record_by_id(pg: &PgPool, request_id: Uuid) -> (i64, i64, i64) {
    for _ in 0..80 {
        if let Some(r) = sqlx::query!(
            r#"SELECT amount_micro, prompt_tokens, completion_tokens
               FROM billing_records WHERE request_id = $1 AND status = 20"#,
            request_id
        )
        .fetch_optional(pg)
        .await
        .unwrap()
        {
            return (
                r.amount_micro,
                i64::from(r.prompt_tokens),
                i64::from(r.completion_tokens),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("8s 内没等到 request_id={request_id} 的结算行");
}

/// 单个计费端点的对账：权威结算行 → 门户日志金额。
async fn check_one_endpoint(
    bed: &Bed,
    client: &reqwest::Client,
    name: &str,
    path: &str,
    body: &Value,
    problems: &mut Vec<String>,
) {
    let resp = client
        .post(format!("http://{}{path}", bed.gateway))
        .bearer_auth(&bed.token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let request_id = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok());
    if status != 200 {
        let text = resp.text().await.unwrap_or_default();
        problems.push(format!(
            "{name} {path} → {status}：{}",
            text.chars().take(160).collect::<String>()
        ));
        return;
    }
    let Some(request_id) = request_id else {
        problems.push(format!(
            "{name} {path}：响应缺 x-okapi-request-id，无从对账"
        ));
        return;
    };
    drop(resp);

    let (amount, _, _) = wait_record_by_id(&bed.pg, request_id).await;
    if amount <= 0 {
        problems.push(format!(
            "{name}：结算金额为 {amount}，这笔没真扣到钱，后面全是假绿"
        ));
        return;
    }

    let (_, logs) = get(bed.console, "/api/me/logs", &bed.token).await;
    let row = logs["data"].as_array().and_then(|rows| {
        rows.iter()
            .find(|r| r["request_id"] == json!(request_id.to_string()))
    });
    match row {
        None => problems.push(format!("{name}：门户日志里找不到 {request_id}")),
        Some(r) if r["amount_micro"] != json!(amount) => problems.push(format!(
            "{name}：门户日志金额 {} ≠ 结算行 {amount}",
            r["amount_micro"]
        )),
        Some(_) => {}
    }
}

/// **每一个**计费端点都要在全部读出口报同一个数。
///
/// `one_request_reports_the_same_amount_on_every_surface` 只驱动了 `/v1/chat/completions`
/// 一个端点——但落结算的是七个模块（chat / embeddings / images / audio / videos /
/// realtime / custom_pass），单笔 chat 对上账不代表另外六个也对得上：它们各自拼
/// `SettlementInput`，字段漏填或填错只有自己那条路径看得见。
///
/// 这里表驱动扫能用简单 HTTP mock 驱动的四条（含两种定价形态：ratio 与 per_call），
/// 每条都走同一套断言：权威行 → 门户日志 → 门户用量 → 生态口径。
/// videos（异步任务）与 realtime（WebSocket）形态不同，各自套件已有专项覆盖，不在此表。
#[tokio::test]
async fn every_billing_endpoint_agrees_across_surfaces() {
    let bed = setup().await;
    let client = reqwest::Client::new();

    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "chat",
            "/v1/chat/completions",
            json!({"model": bed.model, "stream": false,
                   "messages": [{"role": "user", "content": "hello"}]}),
        ),
        (
            "embeddings",
            "/v1/embeddings",
            json!({"model": bed.emb_model, "input": "hello"}),
        ),
        (
            "rerank",
            "/v1/rerank",
            json!({"model": bed.emb_model, "query": "q", "documents": ["a", "b"]}),
        ),
        (
            "images",
            "/v1/images/generations",
            json!({"model": bed.img_model, "prompt": "a cat", "n": 2}),
        ),
        (
            "audio.speech",
            "/v1/audio/speech",
            json!({"model": bed.tts_model, "input": "hello world", "voice": "alloy"}),
        ),
    ];

    let mut problems: Vec<String> = Vec::new();
    for (name, path, body) in &cases {
        check_one_endpoint(&bed, &client, name, path, body, &mut problems).await;
    }

    // 生态口径的累计值 = 本轮全部结算之和（它读 api_keys.used_micro）
    let total: i64 = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(amount_micro), 0)::bigint AS "s!" FROM billing_records
           WHERE user_id = $1 AND status = 20"#,
        bed.user_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    let (_, dash) = get(bed.gateway, "/v1/dashboard/billing/usage", &bed.token).await;
    if dash["total_usage"] != usd_literal(total) {
        problems.push(format!(
            "生态口径 {} ≠ 全部结算之和 {}（{total} micro）",
            dash["total_usage"],
            usd_literal(total)
        ));
    }

    assert!(
        problems.is_empty(),
        "{} 个计费端点对账不一致：\n{}",
        problems.len(),
        problems.join("\n")
    );
}
