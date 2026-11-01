//! 用户的钱与量：用量汇总、余额流水、充值订单。
use super::super::query::Query;
use super::{ch_i64, ch_or_disabled, default_limit};
use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct UsageQuery {
    #[serde(default = "default_days")]
    pub days: u16,
    /// key（默认：当前 key 视角，员工子账户语义）| user（钱包主体汇总）。
    #[serde(default = "default_scope")]
    pub scope: String,
}

pub(super) fn default_days() -> u16 {
    7
}

pub(super) fn default_scope() -> String {
    "key".to_owned()
}

/// GET /api/me/usage：按天用量（CH MV，聚合与请求量解耦）。
pub async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days.clamp(1, 90);

    let sql = match q.scope.as_str() {
        "user" => format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
                    sumMerge(amount) AS amount_micro \
             FROM mv_user_day WHERE user_id = {} AND day >= today() - {days} \
             GROUP BY day ORDER BY day",
            key.user_id
        ),
        _ => format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
                    sumMerge(amount) AS amount_micro \
             FROM mv_apikey_day WHERE api_key_id = {} AND day >= today() - {days} \
             GROUP BY day ORDER BY day",
            key.key_id
        ),
    };
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    let total: i64 = rows.iter().map(|r| ch_i64(r, "amount_micro")).sum();
    Ok(Json(json!({
        "scope": if q.scope == "user" { "user" } else { "key" },
        "days": days,
        "total_amount_micro": total,
        "data": rows,
    })))
}

#[derive(Deserialize)]
pub struct LedgerQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// 游标：取该 event_id 之前的记录。
    #[serde(default)]
    pub before: Option<i64>,
}

/// GET /api/me/ledger：账户流水——余额的**非消费**变动（充值 / 兑换与补偿 /
/// 管理调整 / 退款 / 过期清零），每条带变动后余额。
///
/// 与日志页的分工：日志页是"钱怎么花的"（逐请求，billing_records），这里是
/// "钱怎么来、怎么被动过"（billing_events 里 commit 之外的动账事件）。
/// 网关失败路径也写 `refund` 事件但 delta=0（预扣全额释放、不动账），
/// 用 `delta_micro <> 0` 挡掉——否则每笔上游失败都会在流水里冒一条 $0 退款。
///
/// actor 不原样透出：`admin:42` 对用户只该是"管理员"，管理员 id 属内部信息。
pub async fn ledger(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LedgerQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let limit = q.limit.clamp(1, 200);
    let rows = sqlx::query!(
        r#"SELECT event_id, event_type, delta_micro, balance_after_micro, payload, actor,
                  request_id, created_at, pool
           FROM billing_events
           WHERE user_id = $1
             AND event_type IN ('recharge', 'adjust', 'refund', 'expire',
                                'sub_grant', 'sub_reset', 'sub_expire')
             AND delta_micro <> 0
             AND ($2::bigint IS NULL OR event_id < $2)
           ORDER BY event_id DESC LIMIT $3"#,
        key.user_id,
        q.before,
        limit
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let next_before = rows.last().map(|r| r.event_id);
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let tags: Vec<String> = r
                .payload
                .as_ref()
                .and_then(|p| p.get("tags"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "event_id": r.event_id,
                "event_type": r.event_type,
                "delta_micro": r.delta_micro,
                "balance_after_micro": r.balance_after_micro,
                // 0 钱包 1 订阅池（§11.28）：订阅事件的 balance_after 是池余额，不是钱包
                "pool": r.pool,
                "source": ledger_source(&r.actor, &tags, r.pool),
                "tags": tags,
                // 退款锚到具体请求：用户可去日志页核对被退的那一笔
                "request_id": r.request_id,
                "created_at": r.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "next_before": next_before })))
}

/// actor + tags → 用户可读的来源枚举（前端按枚举映射文案，§8 后端不拼人类语言）。
pub(super) fn ledger_source(actor: &str, tags: &[String], pool: i16) -> &'static str {
    if tags.iter().any(|t| t == "aff_rebate") {
        return "aff";
    }
    match actor.split(':').next().unwrap_or_default() {
        // MCP 写工具经管理员 key 操作，对用户而言同为"管理员操作"
        "admin" | "mcp" => "admin",
        "system" => match actor {
            "system:payment" => "payment",
            "system:redeem" => "redeem",
            "system:aff" => "aff",
            // worker 既做余额到期也做订阅滚窗 / 到期（pool=1）：后者由 event_type 自述，
            // 来源只标"系统"，别把额度重置也写成"余额到期"
            "system:worker" if pool == 1 => "system",
            "system:worker" => "expiry",
            a if a.starts_with("system:migrate") => "migration",
            _ => "system",
        },
        _ => "system",
    }
}

/// GET /api/me/orders：我的充值订单（recharge_orders，含未支付/失败——
/// 流水里只有已支付成功的那条 recharge 事件，用户找"我付了钱怎么没到账"
/// 要看的是订单状态而非流水）。
pub async fn orders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LedgerQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let limit = q.limit.clamp(1, 200);
    let rows = sqlx::query!(
        r#"SELECT id, order_no, amount_micro, currency, pay_amount::text AS "pay_amount?",
                  gateway, status, paid_at, created_at
           FROM recharge_orders
           WHERE user_id = $1 AND ($2::bigint IS NULL OR id < $2)
           ORDER BY id DESC LIMIT $3"#,
        key.user_id,
        q.before,
        limit
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let next_before = rows.last().map(|r| r.id);
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "order_no": r.order_no,
                "amount_micro": r.amount_micro,
                "currency": r.currency,
                // 原币种支付金额按 NUMERIC 文本透出（展示层再格式化，不走浮点）
                "pay_amount": r.pay_amount,
                "gateway": r.gateway,
                // 0 created 1 paid 2 failed 3 refunded
                "status": r.status,
                "paid_at": r.paid_at.map(|t| t.to_rfc3339()),
                "created_at": r.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "next_before": next_before })))
}
