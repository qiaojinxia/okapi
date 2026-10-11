//! 用户门户 API（M2 第一批）：余额 / 用量 / key 分账。
//!
//! 合作商轻量模式（IMPLEMENTATION §6.1 的 key 即子账户）：给每位员工发独立 key，
//! `/api/me/usage` 默认 `scope=key`（员工只看自己这把 key 的用量），
//! `scope=user` 为钱包主体汇总视图。完整 Team 层（独立登录/成员限额）在 M4。
//! 统计查询走 ClickHouse MV；未启用 CH 时 fail-closed 返回 501 stats_disabled。

mod key_trends;
mod pricing;
mod usage_logs;
pub use pricing::{public_groups, public_models, public_pricing, public_statistics};
pub use usage_logs::{list as logs, series as logs_series, stat as logs_stat};

use super::query::{PageQuery, Query};
use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;

fn ch_or_disabled(state: &AppState) -> Result<&ChClient, AppError> {
    state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))
}

fn ch_i64(row: &Value, key: &str) -> i64 {
    row.get(key).map_or(0, |v| {
        v.as_str()
            .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
            .unwrap_or(0)
    })
}

/// GET /api/me：身份、余额与**生效权限点**（热余额为准，快照列对账用）。
///
/// 权限点给前端用来决定"哪些入口该出现"，而不是让用户点进去再吃 403——
/// 一个只读运维角色看到十个改配置的按钮，每个都点不动，是很糟的体验。
/// 语义与后端 `AuthedKey::has_permission` 一致：`["*"]` 表示全权。
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let balance = state.ledger.balance(key.user_id).await?;
    let has_web_session = super::auth_web::require_session(&state, &headers)
        .await
        .is_ok_and(|user_id| user_id == key.actor_user_id());
    let key_info: Option<(String, String, String)> =
        sqlx::query_as("SELECT k.name, k.key_prefix, u.username FROM api_keys k JOIN users u ON u.id = COALESCE(k.member_user_id,k.user_id) WHERE k.id = $1 AND k.user_id = $2")
            .bind(key.key_id)
            .bind(key.user_id)
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    // 订阅池（§11.28）：热值一次 HMGET；窗口外 / 越界为负对用户都显示 0
    let (sub, sub_until) = state.ledger.sub_balance(key.user_id).await?;
    let sub_active = sub_until > chrono::Utc::now().timestamp();
    // 余额有效期（#1790-6）是本站独有的机制：钱会在某一天被清零，用户必须能在
    // 首页看到那一天——不在鉴权缓存里（低频字段），点查 PG 一次。
    let balance_expires_at = sqlx::query_scalar!(
        r#"SELECT balance_expires_at FROM users WHERE id = $1"#,
        key.user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .flatten();
    // super_admin 与"未绑定自定义角色的 admin"都是全权（对齐 new-api 迁移习惯）
    let permissions: Vec<String> = if key.role >= 100 {
        vec!["*".to_owned()]
    } else if key.role >= 10 {
        key.permissions
            .clone()
            .unwrap_or_else(|| vec!["*".to_owned()])
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "user_id": key.actor_user_id(),
        "wallet_user_id": key.user_id,
        "username": key_info.as_ref().map(|info| &info.2),
        "key_id": key.key_id,
        "key_name": key_info.as_ref().map(|info| &info.0),
        "key_prefix": key_info.as_ref().map(|info| &info.1),
        "has_web_session": has_web_session,
        "group": key.group_code,
        "balance_micro": balance.as_micros(),
        "balance_expires_at": balance_expires_at.map(|t| t.to_rfc3339()),
        // 订阅池剩余（窗口内才有；无订阅 / 窗口外 = 0）与池可用截止 unix 秒（0 = 无）
        "subscription_remaining_micro": if sub_active { sub.as_micros().max(0) } else { 0 },
        "subscription_until_unix": if sub_active { sub_until } else { 0 },
        "role": key.role,
        "permissions": permissions,
    })))
}

#[derive(Deserialize)]
pub struct UsageQuery {
    #[serde(default = "default_days")]
    pub days: u16,
    /// key（默认：当前 key 视角，员工子账户语义）| user（钱包主体汇总）。
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_days() -> u16 {
    7
}

fn default_scope() -> String {
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
pub struct LogsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// 游标：上一页最后一行的 id；按 (created_at, id) 倒序取它之后的记录。
    #[serde(default)]
    pub before: Option<i64>,
    /// `key`（缺省）| `user`：与 /api/me/usage 同一语义——合作商员工缺省只见自己那把 key。
    #[serde(default)]
    pub scope: Option<String>,
    /// 精确模型名过滤。
    #[serde(default)]
    pub model: Option<String>,
    /// 只看失败（status = 40，不包含已退款记录）。
    #[serde(default)]
    pub errors_only: Option<bool>,
    pub api_key_id: Option<i64>,
    pub request_id: Option<uuid::Uuid>,
    /// 含首尾的日历日期，与门户看板下钻携带的统计时区配套。
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub timezone: Option<String>,
}

/// 门户日志列表与汇总的缺省回看天数（含今天）。
const DEFAULT_LOG_DAYS: u32 = 30;

struct LogWindow {
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
    timezone: String,
}

impl LogsQuery {
    /// 没给日期时缺省近 [`DEFAULT_LOG_DAYS`] 天：账单表按 created_at 分区，无时间界的列表 / 汇总
    /// 要扫用户全部历史分区，成本随终身账单行数线性涨（保留期缺省永久）。按 request_id 点查不加界。
    async fn window(&self, pg: &sqlx::PgPool) -> Result<Option<LogWindow>, AppError> {
        if self.start_date.is_none() && self.end_date.is_none() && self.request_id.is_some() {
            return Ok(None);
        }
        let timezone = self.timezone.as_deref().unwrap_or("UTC");
        if timezone.len() > 128 {
            return Err(AppError::bad_request().with_param("timezone"));
        }
        // PG 校验 IANA 时区并提供当地今天；不依赖 ClickHouse，也不使用浏览器时钟。
        let today = sqlx::query_scalar::<_, chrono::NaiveDate>(
            "SELECT (CURRENT_TIMESTAMP AT TIME ZONE name)::date FROM pg_timezone_names WHERE name = $1",
        )
        .bind(timezone)
        .fetch_optional(pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::bad_request().with_param("timezone"))?;
        let (start, end) = super::usage_details::CalendarWindow::bounds(
            today,
            DEFAULT_LOG_DAYS,
            self.start_date.as_deref(),
            self.end_date.as_deref(),
        )?;
        Ok(Some(LogWindow {
            start,
            end,
            timezone: timezone.to_owned(),
        }))
    }
}

fn default_limit() -> i64 {
    50
}

/// GET /api/notice：站点公告（无鉴权，登录页也要显示）。
///
/// 存 `settings.site_notice`（吸收判据②：能用现有表表达就不新增表），经 60s 进程缓存
/// 读取——发布后一分钟内全站可见，足够。对外只透出四个白名单字段并做类型收口：
/// settings 的写入口是泛型 key/value，不能假设值形状；`level` 收敛到三档枚举，
/// 正文截断到 4000 字——公告是横幅，不是文章。
pub async fn notice(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let raw = state.setting_cached("site_notice").await;
    let Some(v) = raw.as_ref() else {
        return Ok(Json(json!({ "notice": Value::Null })));
    };
    let enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let body = v
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !enabled || body.is_empty() {
        return Ok(Json(json!({ "notice": Value::Null })));
    }
    let level = match v.get("level").and_then(Value::as_str) {
        Some("warning") => "warning",
        Some("critical") => "critical",
        _ => "info",
    };
    let clipped: String = body.chars().take(4000).collect();
    Ok(Json(json!({
        "notice": {
            "title": v.get("title").and_then(Value::as_str).unwrap_or_default().trim(),
            "body": clipped,
            "level": level,
            // 前端用它做"已读"锚点：重新发布（updated_at 变）会再次弹出
            "updated_at": v.get("updated_at").and_then(Value::as_str).unwrap_or_default(),
        }
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
fn ledger_source(actor: &str, tags: &[String], pool: i16) -> &'static str {
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

#[derive(Deserialize)]
pub struct RedeemReq {
    pub code: String,
}

/// POST /api/me/redeem：兑换码核销（行级原子，一次性；credit 事件 actor=system:redeem）。
pub async fn redeem(
    State(state): State<AppState>,
    conn: crate::console::auth_web::MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<RedeemReq>,
) -> Result<Json<Value>, AppError> {
    // 兑换码爆破面：每 IP 限速（对齐 new-api rc.24 关键路由限流）
    crate::console::auth_web::critical_rate_guard(&state, &headers, conn.0.as_ref(), "redeem", 10)
        .await?;
    let key = authenticate(&state, &headers).await?;
    let code = req.code.trim();

    // per-IP 闸（#1790-5）：翻转前预查批次限额；IP 取 CDN 头（直连无头不限）
    let precheck = okapi_store::admin::redemption_precheck(&state.pg, code).await?;
    let mut ip_charge: Option<(uuid::Uuid, String)> = None;
    if let Some(pre) = &precheck
        && let Some(cap) = pre.max_per_ip
        && let Some(ip) = crate::gateway::clients::detect_client_ip(&headers)
    {
        let count = state.sched.redeem_ip_incr(pre.batch_id, &ip).await;
        if count > i64::from(cap) {
            state.sched.redeem_ip_decr(pre.batch_id, &ip).await;
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                okapi_api::codes::RATE_LIMITED,
            )
            .with_param("redeem_ip"));
        }
        ip_charge = Some((pre.batch_id, ip));
    }

    // 走到这里本 IP 已经计了一次。没核销成功（拿锁、开事务、翻转、入账、提交任何一步失败）
    // 都退还，否则一次库故障就占掉这个 IP 7 天的配额；提交之后再失败不退，码已经用掉了。
    let (mut guard, claimed, accepted) = match accept_redemption(&state, key.user_id, code).await {
        Ok(accepted) => accepted,
        Err(error) => {
            if let Some((batch, ip)) = &ip_charge {
                state.sched.redeem_ip_decr(*batch, ip).await;
            }
            return Err(error);
        }
    };
    match accepted {
        Accepted::Subscription(id) => {
            let receipt =
                okapi_ledger::subscriptions::finish(&mut guard, &state.ledger, key.user_id, id)
                    .await;
            drop(guard);
            state.sched.auth_flush().await;
            let mut body = super::subscriptions::receipt_view(&state, &receipt).await?;
            body["amount_micro"] = json!(0);
            body["plan_code"] = json!(claimed.plan_code);
            Ok(Json(body))
        }
        Accepted::Wallet(operation_id) => {
            let receipt = okapi_ledger::transfers::finish(
                &mut guard,
                &state.ledger,
                key.user_id,
                operation_id,
                okapi_ledger::Pool::Wallet,
            )
            .await;
            Ok(Json(json!({
                "amount_micro": claimed.amount_micro,
                "balance_after_micro": receipt.balance_after.map(okapi_domain::Money::as_micros),
                "operation_id": receipt.operation_id,
                "pending": receipt.balance_after.is_none(),
                "plan_code": claimed.plan_code,
                "granted_group": claimed.grant_group,
                "balance_valid_days": claimed.balance_valid_days,
            })))
        }
    }
}

/// 兑换码在事务里落定的去处：订阅入队（激活单号）或钱包入账（操作号）。
enum Accepted {
    Subscription(uuid::Uuid),
    Wallet(uuid::Uuid),
}

/// 锁用户、翻转兑换码并在同一事务里入账，一直到提交。返回 Err 时兑换码没有被用掉。
async fn accept_redemption(
    state: &AppState,
    user_id: i64,
    code: &str,
) -> Result<
    (
        okapi_ledger::holds::UserGuard,
        okapi_store::admin::ClaimedRedemption,
        Accepted,
    ),
    AppError,
> {
    let mut guard = okapi_ledger::holds::UserGuard::acquire(&state.pg, user_id).await?;
    let mut tx = guard
        .connection()?
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let Some(mut claimed) =
        okapi_store::admin::claim_redemption_in_tx(&mut tx, code, user_id).await?
    else {
        // 预查通过但翻转失败（竞争被抢 / 绑定他人）
        return Err(AppError::new(StatusCode::NOT_FOUND, "redemption_invalid"));
    };

    // 绑订阅套餐（§11.28）：核销即激活 / 续期，钱包不动、面值忽略
    let accepted = if claimed.subscription_plan_id.is_some() {
        let plan: okapi_store::subscriptions::SubPlan = serde_json::from_value(
            claimed
                .subscription_snapshot
                .take()
                .ok_or_else(AppError::internal)?,
        )
        .map_err(|_| AppError::internal())?;
        let enqueued = okapi_ledger::subscriptions::enqueue(
            &mut tx,
            user_id,
            &plan,
            &format!("redeem:{}", claimed.code_id),
            "system:redeem",
            false,
        )
        .await;
        match enqueued {
            Ok(id) => Accepted::Subscription(id),
            Err(error) => {
                tx.rollback().await.map_err(okapi_store::StoreError::from)?;
                return Err(error.into());
            }
        }
    } else {
        Accepted::Wallet(accept_wallet_redemption(&mut tx, user_id, &claimed).await?)
    };
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    Ok((guard, claimed, accepted))
}

/// Keep optional plan benefits in the same transaction as code consumption.
async fn accept_wallet_redemption(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: i64,
    claimed: &okapi_store::admin::ClaimedRedemption,
) -> Result<uuid::Uuid, AppError> {
    let amount = okapi_domain::Money::from_micros(claimed.amount_micro);
    let operation_id = okapi_ledger::transfers::credit_in_tx(
        tx,
        user_id,
        amount,
        "adjust",
        "system:redeem",
        json!({"tags": ["redemption"], "code_id": claimed.code_id, "plan_code": claimed.plan_code}),
    )
    .await?;
    if let Some(group) = &claimed.grant_group {
        sqlx::query!("INSERT INTO user_groups(user_id,group_code,priority) VALUES ($1,$2,0) ON CONFLICT (user_id,group_code) DO NOTHING",user_id,group)
            .execute(&mut **tx).await.map_err(okapi_store::StoreError::from)?;
    }
    if let Some(days) = claimed.balance_valid_days {
        let expires = chrono::Utc::now() + chrono::Duration::days(i64::from(days));
        sqlx::query!(
            "UPDATE users SET balance_expires_at=$2,updated_at=now() WHERE id=$1",
            user_id,
            expires
        )
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    Ok(operation_id)
}

/// 用户可为自己的 key 选择的分组（IMPLEMENTATION §11.14 R4，对齐 new-api UserUsableGroups）：
/// 管理员分配给他的组 ∪ 标为 `self_select` 的公开档位 ∪ 默认组。
/// 价随组走：选了 vip 就按 vip 倍率计费、走 vip 池——这是产品层的"自选套餐档位"。
pub(super) struct SelectableGroup {
    pub code: String,
    pub ratio: String,
    pub description: Option<String>,
    /// assigned | self_select | default
    pub source: &'static str,
}

pub(super) async fn selectable_groups(
    state: &AppState,
    user_id: i64,
) -> Result<Vec<SelectableGroup>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT g.group_code, g.group_ratio::text AS "ratio!", g.description, g.is_default,
                  g.self_select,
                  EXISTS(SELECT 1 FROM user_groups ug
                          WHERE ug.user_id = $1 AND ug.group_code = g.group_code) AS "assigned!"
           FROM price_groups g
           WHERE g.self_select OR g.is_default
              OR EXISTS(SELECT 1 FROM user_groups ug
                         WHERE ug.user_id = $1 AND ug.group_code = g.group_code)
           ORDER BY g.sort_order, g.group_code"#,
        user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(rows
        .into_iter()
        .map(|r| SelectableGroup {
            source: if r.assigned {
                "assigned"
            } else if r.self_select {
                "self_select"
            } else {
                "default"
            },
            code: r.group_code,
            ratio: r.ratio,
            description: r.description,
        })
        .collect())
}

/// 校验用户想给 key 选的分组是否在可选集合内；不在 → 403 `group_not_selectable`。
/// 不是 404：组可能存在，只是他没资格选——两种事要分开说。
pub(super) async fn ensure_selectable(
    state: &AppState,
    user_id: i64,
    group_code: &str,
) -> Result<(), AppError> {
    let ok = selectable_groups(state, user_id)
        .await?
        .iter()
        .any(|g| g.code == group_code);
    if ok {
        Ok(())
    } else {
        Err(
            AppError::new(StatusCode::FORBIDDEN, codes::GROUP_NOT_SELECTABLE)
                .with_param(group_code.to_owned()),
        )
    }
}

/// GET /api/me/groups：我能选的分组 + 当前生效分组。
/// 门户建 key / 改 key 的档位下拉据此渲染；空的 self_select 站点只会看到自己被分配的组。
pub async fn groups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let list = selectable_groups(&state, key.user_id).await?;
    Ok(Json(json!({
        "current": key.group_code,
        "data": list.iter().map(|g| json!({
            "code": g.code, "ratio": g.ratio, "description": g.description, "source": g.source,
        })).collect::<Vec<_>>(),
    })))
}

/// GET /api/me/keys：本用户的 key 及累计分账（合作商查员工用量）；`limit/offset` 可选切片，
/// 默认每页 20 条并返回总数。
pub async fn keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let slice = q.slice();
    let delegated_key = key.member_user_id.map(|_| key.key_id);
    // 登录 key（session_hash 非空）不在这里：它们随登录会话在「登录设备」里管理
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT id, name, key_prefix, status, used_micro, rpm_limit, tpm_limit, rpd_limit,
               daily_token_limit, max_concurrency, model_allowlist, group_override, ip_allowlist,
               expires_at, last_used_at, created_at
        FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL AND session_hash IS NULL
          AND ($4::bigint IS NULL OR id=$4) ORDER BY id
        LIMIT $2 OFFSET $3
        "#,
            key.user_id,
            slice.limit,
            slice.offset,
            delegated_key
        )
        .fetch_all(&state.pg),
        okapi_store::listing::count_unless_all(
            slice,
            sqlx::query_scalar!(
                r#"SELECT COUNT(*)::bigint AS "c!" FROM api_keys
           WHERE user_id = $1 AND deleted_at IS NULL AND session_hash IS NULL
             AND ($2::bigint IS NULL OR id=$2)"#,
                key.user_id,
                delegated_key
            )
            .fetch_one(&state.pg)
        ),
    )
    .map_err(okapi_store::StoreError::from)?;
    let total = total.unwrap_or_else(|| okapi_store::listing::len_as_total(rows.len()));

    let key_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let quotas = key_quotas(&state.pg, key.user_id, &key_ids).await?;
    let saved_ids = okapi_store::api_key_secret::saved_ids(&state.pg, key.user_id, &key_ids)
        .await
        .map_err(|err| tracing::warn!(error = %err, "key copy metadata unavailable"))
        .ok();
    let trends = key_trends::load(
        &state.pg,
        key.user_id,
        &key_ids,
        chrono::Utc::now().date_naive(),
    )
    .await
    .map_err(|err| tracing::warn!(error = %err, "key Token trends unavailable"))
    .ok();

    let ch_usage = key_aggregate_usage(state.ch.as_ref(), &key_ids).await;

    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let agg = ch_usage.iter().find(|u| ch_i64(u, "api_key_id") == r.id);
            json!({
                "id": r.id,
                "name": r.name,
                "key_prefix": r.key_prefix,
                "copy_status": match &saved_ids {
                    Some(ids) if !ids.contains(&r.id) => "not_saved",
                    Some(_) if state.master_key.is_some() => "available",
                    _ => "unavailable",
                },
                "status": r.status,
                "used_micro": r.used_micro,
                "quota_mode": quotas.get(&r.id).map_or(0, |q| q.0),
                "quota_micro": quotas.get(&r.id).and_then(|q| q.1),
                "rpm_limit": r.rpm_limit,
                "tpm_limit": r.tpm_limit,
                "rpd_limit": r.rpd_limit,
                "daily_token_limit": r.daily_token_limit,
                "max_concurrency": r.max_concurrency,
                "model_allowlist": r.model_allowlist,
                "group_override": r.group_override,
                "ip_allowlist": r.ip_allowlist,
                "expires_at": r.expires_at,
                "last_used_at": r.last_used_at,
                "created_at": r.created_at,
                "amount_micro": agg.map_or(0, |u| ch_i64(u, "amount_micro")),
                "requests": agg.map_or(0, |u| ch_i64(u, "requests")),
                "usage_trend": trends.as_ref().and_then(|trends| trends.get(&r.id)),
            })
        })
        .collect();
    Ok(Json(
        json!({ "data": data, "total": total, "key_limits_supported": true }),
    ))
}

async fn key_quotas(
    pg: &sqlx::PgPool,
    user_id: i64,
    ids: &[i64],
) -> Result<std::collections::BTreeMap<i64, (i16, Option<i64>)>, AppError> {
    let rows: Vec<(i64, i16, Option<i64>)> = sqlx::query_as(
        "SELECT id, quota_mode, quota_micro FROM api_keys WHERE user_id=$1 AND id=ANY($2)",
    )
    .bind(user_id)
    .bind(ids)
    .fetch_all(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(rows
        .into_iter()
        .map(|(id, mode, quota)| (id, (mode, quota)))
        .collect())
}

/// CH unavailable preserves the existing PG-only key listing behavior.
async fn key_aggregate_usage(ch: Option<&okapi_store::ChClient>, ids: &[i64]) -> Vec<Value> {
    let Some(ch) = ch.filter(|_| !ids.is_empty()) else {
        return Vec::new();
    };
    let ids = ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT api_key_id, sumMerge(amount) AS amount_micro, countMerge(requests) AS requests FROM mv_apikey_day WHERE api_key_id IN ({ids}) GROUP BY api_key_id"
    );
    ch.query_json_each_row(&sql).await.unwrap_or_default()
}

/// 自助面可改字段：收窄自己这把 key（名字 / 状态 / 过期 / 白名单），外加**在可选集合内**
/// 换分组——分组是计价锚点，但可选集合由管理员通过 `self_select` 与用户分组划定，
/// 用户只能在划定的分组里挑，不构成自行改价；密钥额度不改变账户余额。
#[derive(Deserialize)]
pub struct PatchKeyReq {
    #[serde(default, deserialize_with = "super::double_option")]
    pub quota_micro: Option<Option<i64>>,
    #[serde(default)]
    pub name: Option<String>,
    /// 1=启用 2=停用。
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub expires_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    /// 字符串数组；null = 解除模型限制。
    #[serde(default, deserialize_with = "super::double_option")]
    pub model_allowlist: Option<Option<Vec<String>>>,
    /// 分组：字符串 = 选定分组（须在 /api/me/groups 可选集合内）；null = 跟随用户分组。
    #[serde(default, deserialize_with = "super::double_option")]
    pub group_code: Option<Option<String>>,
    /// IP 白名单：地址或 CIDR 数组；null / 空数组 = 解除限制。每条须可解析，否则 400——
    /// 一条拼错的白名单会把 key 锁死而毫无提示。
    #[serde(default, deserialize_with = "super::double_option")]
    pub ip_allowlist: Option<Option<Vec<String>>>,
}

/// IP 白名单归一化：去空白、去重、逐条校验；空数组等价于"不限"（存 null）。
pub(super) fn normalize_ip_allowlist(list: Option<Vec<String>>) -> Result<Option<Value>, AppError> {
    let Some(list) = list else {
        return Ok(None);
    };
    let mut out: Vec<String> = Vec::new();
    for raw in list {
        let entry = raw.trim().to_owned();
        if entry.is_empty() || out.contains(&entry) {
            continue;
        }
        if !okapi_store::netmatch::is_valid_entry(&entry) {
            return Err(AppError::bad_request().with_param(format!("ip_allowlist:{entry}")));
        }
        out.push(entry);
    }
    Ok((!out.is_empty()).then(|| json!(out)))
}

/// 模型白名单归一化：空数组等价于"不限"，避免落成一把谁也调不通的死 key。
pub(super) fn normalize_allowlist(list: Option<Vec<String>>) -> Option<Value> {
    let items: Vec<String> = list?
        .into_iter()
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty())
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(json!(items))
}

pub(super) fn validate_key_limits(
    quota: Option<i64>,
    expires: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), AppError> {
    if quota.is_some_and(|v| !(1..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&v)) {
        return Err(AppError::bad_request().with_param("quota_micro"));
    }
    if expires.is_some_and(|v| v <= chrono::Utc::now()) {
        return Err(AppError::bad_request().with_param("expires_at"));
    }
    Ok(())
}

/// PATCH /api/me/keys/{id}：改自己 key 的名称/启停/过期/模型白名单。
pub async fn patch_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(req): ExtractJson<PatchKeyReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    // A delegated team credential manages itself. Owner/admin management of
    // other members uses the session-authenticated /api/teams endpoints.
    if key.member_user_id.is_some() && id != key.key_id {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::PERMISSION_DENIED,
        ));
    }
    validate_key_limits(req.quota_micro.flatten(), req.expires_at.flatten())?;
    // A delegated/restricted credential must not remove its own restrictions.
    let changes_access = req.quota_micro.is_some()
        || req.expires_at.is_some()
        || req.model_allowlist.is_some()
        || req.group_code.is_some()
        || req.ip_allowlist.is_some();
    if changes_access
        && (key.quota_limited
            || key.model_allowlist.is_some()
            || key.expires_at.is_some()
            || key.ip_allowlist.is_some())
        && super::auth_web::require_session(&state, &headers)
            .await
            .ok()
            != Some(key.user_id)
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "key_limits_session_required",
        ));
    }
    if let Some(status) = req.status
        && !matches!(status, 1 | 2)
    {
        return Err(AppError::bad_request().with_param("status"));
    }
    // 停用当前登录用的这把 key = 把自己锁在门外（同删除）
    if req.status == Some(2) && id == key.key_id {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            codes::CURRENT_KEY_IN_USE,
        ));
    }
    let group_override = match req.group_code {
        Some(Some(code)) => {
            let code = code.trim().to_owned();
            ensure_selectable(&state, key.user_id, &code).await?;
            Some(Some(code))
        }
        Some(None) => Some(None),
        None => None,
    };
    let ip_allowlist = match req.ip_allowlist {
        Some(list) => Some(normalize_ip_allowlist(list)?),
        None => None,
    };
    let patch = okapi_store::admin::ApiKeyPatch {
        quota_micro: req.quota_micro,
        name: req.name.map(|n| n.trim().to_owned()),
        status: req.status,
        expires_at: req.expires_at,
        model_allowlist: req.model_allowlist.map(normalize_allowlist),
        group_override,
        ip_allowlist,
        ..Default::default()
    };
    let touched =
        okapi_store::admin::patch_api_key(&state.pg, id, Some(key.user_id), &patch).await?;
    let Some(touched) = touched else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    // 先落库后失效：并发回源读到的必是新值
    state.sched.auth_del(&touched.key_hash).await;
    Ok(Json(json!({ "ok": true, "key_id": id })))
}

/// DELETE /api/me/keys/{id}：吊销自己的 key（软删除，明文 key 立即失效）。
/// 当前请求自己用的那把不能删：删了当前登录立刻失效，界面上所有请求都会变成 invalid_api_key。
pub async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    if id == key.key_id {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            codes::CURRENT_KEY_IN_USE,
        ));
    }
    // A delegated team credential manages itself. Owner/admin management of
    // other members uses the session-authenticated /api/teams endpoints.
    if key.member_user_id.is_some() && id != key.key_id {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::PERMISSION_DENIED,
        ));
    }
    let touched = okapi_store::admin::soft_delete_api_key(&state.pg, id, Some(key.user_id)).await?;
    let Some(touched) = touched else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    state.sched.auth_del(&touched.key_hash).await;
    Ok(Json(json!({ "ok": true, "key_id": id })))
}

/// GET /api/me/aff：邀请码（惰性生成）+ 邀请人数 + 累计返利（M4 aff）。
pub async fn aff(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let code = sqlx::query_scalar!(
        r#"SELECT aff_code FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        key.user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .flatten();
    let code = if let Some(code) = code {
        code
    } else {
        // 惰性生成：8 位小写字母数字；唯一索引冲突重试
        loop {
            let candidate: String = {
                use rand::RngExt;
                use rand::distr::Alphanumeric;
                rand::rng()
                    .sample_iter(&Alphanumeric)
                    .take(8)
                    .map(|c| (c as char).to_ascii_lowercase())
                    .collect()
            };
            let updated = sqlx::query!(
                r#"UPDATE users SET aff_code = $2 WHERE id = $1 AND aff_code IS NULL"#,
                key.user_id,
                candidate
            )
            .execute(&state.pg)
            .await;
            match updated {
                Ok(r) if r.rows_affected() == 1 => break candidate,
                Ok(_) => {
                    // 并发已生成：读回
                    if let Ok(Some(Some(existing))) = sqlx::query_scalar!(
                        r#"SELECT aff_code FROM users WHERE id = $1"#,
                        key.user_id
                    )
                    .fetch_optional(&state.pg)
                    .await
                    {
                        break existing;
                    }
                }
                Err(_) => {} // 唯一冲突：换码重试
            }
        }
    };

    let invitees = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM users WHERE inviter_id = $1 AND deleted_at IS NULL"#,
        key.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut history = okapi_store::history::read(&state.pg).await?;
    let reward_sum = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(delta_micro), 0)::bigint AS "s!"
           FROM billing_actor_totals WHERE user_id = $1 AND actor = 'system:aff'"#,
        key.user_id
    )
    .fetch_one(&mut *history)
    .await
    .map_err(okapi_store::StoreError::from)?;

    history
        .commit()
        .await
        .map_err(okapi_store::StoreError::from)?;

    Ok(Json(json!({
        "aff_code": code,
        "invitees": invitees,
        "reward_sum_micro": reward_sum,
    })))
}
