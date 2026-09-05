//! 用户门户 API（M2 第一批）：余额 / 用量 / key 分账。
//!
//! 合作商轻量模式（IMPLEMENTATION §6.1 的 key 即子账户）：给每位员工发独立 key，
//! `/api/me/usage` 默认 `scope=key`（员工只看自己这把 key 的用量），
//! `scope=user` 为钱包主体汇总视图。完整 Team 层（独立登录/成员限额）在 M4。
//! 统计查询走 ClickHouse MV；未启用 CH 时 fail-closed 返回 501 stats_disabled。

use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};

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
        "user_id": key.user_id,
        "key_id": key.key_id,
        "group": key.group_code,
        "balance_micro": balance.as_micros(),
        "balance_expires_at": balance_expires_at.map(|t| t.to_rfc3339()),
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

#[derive(sqlx::FromRow)]
struct PublicPricingModel {
    model_name: String,
    display_name: Option<String>,
    vendor: Option<String>,
    capabilities: Value,
    context_window: Option<i32>,
    max_output: Option<i32>,
    pricing_mode: String,
    model_ratio: Option<String>,
    completion_ratio: Option<String>,
    cache_ratio: Option<String>,
    cache_write_ratio: Option<String>,
    audio_ratio: Option<String>,
    audio_completion_ratio: Option<String>,
    image_ratio: Option<String>,
    per_call_price_micro: Option<i64>,
}

/// GET /api/pricing：公开模型规格、价格和分组可见性；不含渠道或成本信息。
pub async fn public_pricing(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let models = sqlx::query_as::<_, PublicPricingModel>(
        r"SELECT m.model_name, m.display_name, m.vendor, p.pricing_mode,
                  m.capabilities, m.context_window, m.max_output,
                  p.model_ratio::text AS model_ratio,
                  p.completion_ratio::text AS completion_ratio,
                  p.cache_ratio::text AS cache_ratio,
                  p.cache_write_ratio::text AS cache_write_ratio,
                  p.audio_ratio::text AS audio_ratio,
                  p.audio_completion_ratio::text AS audio_completion_ratio,
                  p.image_ratio::text AS image_ratio,
                  p.per_call_price_micro
           FROM models m JOIN model_pricing p ON p.model_id = m.id
           WHERE m.status = 1 ORDER BY m.sort_order, m.model_name",
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let groups = sqlx::query!(
        r#"SELECT g.group_code AS code, g.description AS name, g.group_ratio::text AS ratio,
                  g.pool_code, g.self_select, p.fallback_pool_code
           FROM price_groups g
           LEFT JOIN channel_pools p ON p.pool_code = g.pool_code
           ORDER BY g.sort_order, g.group_code"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    // 可见性事实：启用渠道声称服务的 (模型, 所在池) 对。渠道只服务它所在的池，
    // 分组经其池链（主池 → 降级池）能到达的池里有人服务该模型即"可用"——
    // 与 candidates_for_model 同一套规则的静态视图；key 级健康属瞬态，价格页不看。
    let served = sqlx::query!(
        r#"SELECT DISTINCT mn.name AS "model_name!", pc.pool_code AS "pool_code!"
           FROM channels c
           CROSS JOIN LATERAL jsonb_array_elements_text(c.models) AS mn(name)
           JOIN pool_channels pc ON pc.channel_id = c.id
           WHERE c.status = 1 AND c.deleted_at IS NULL"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut pools_of: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for row in &served {
        pools_of
            .entry(row.model_name.as_str())
            .or_default()
            .push(row.pool_code.as_str());
    }
    let usable_groups = |model: &str| -> Vec<&str> {
        let Some(pools) = pools_of.get(model) else {
            return Vec::new();
        };
        groups
            .iter()
            .filter(|g| {
                pools.contains(&g.pool_code.as_str())
                    || g.fallback_pool_code
                        .as_deref()
                        .is_some_and(|fb| pools.contains(&fb))
            })
            .map(|g| g.code.as_str())
            .collect()
    };

    Ok(Json(json!({
        "models": models.iter().map(|m| json!({
            "model": m.model_name,
            "display_name": m.display_name,
            "vendor": m.vendor,
            // 公开目录仅发已声明的布尔能力，不透传管理员可能存入的扩展数据。
            "capabilities": (["vision", "tools", "json", "reasoning", "audio", "video", "embedding", "realtime"]
                .into_iter().filter_map(|key| m.capabilities.get(key).and_then(Value::as_bool)
                    .map(|value| (key.to_owned(), json!(value))))
                .collect::<serde_json::Map<String, Value>>()),
            "context_window": m.context_window.filter(|v| *v > 0),
            "max_output": m.max_output.filter(|v| *v > 0),
            "mode": m.pricing_mode,
            "model_ratio": m.model_ratio,
            "completion_ratio": m.completion_ratio,
            "cache_ratio": m.cache_ratio,
            "cache_write_ratio": m.cache_write_ratio,
            "audio_ratio": m.audio_ratio,
            "audio_completion_ratio": m.audio_completion_ratio,
            "image_ratio": m.image_ratio,
            "per_call_price_micro": m.per_call_price_micro,
            "groups": usable_groups(&m.model_name),
        })).collect::<Vec<_>>(),
        "groups": groups.iter().map(|g| json!({
            "code": g.code, "name": g.name, "ratio": g.ratio, "self_select": g.self_select,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// 游标：取该 id 之前的记录（created_at 倒序翻页）。
    #[serde(default)]
    pub before: Option<i64>,
    /// `key`（缺省）| `user`：与 /api/me/usage 同一语义——合作商员工缺省只见自己那把 key。
    #[serde(default)]
    pub scope: Option<String>,
    /// 精确模型名过滤。
    #[serde(default)]
    pub model: Option<String>,
    /// 只看失败（status ≠ 20 的记录：上游失败/空回复/拒绝）。
    #[serde(default)]
    pub errors_only: Option<bool>,
}

fn default_limit() -> i64 {
    50
}

/// GET /api/me/logs：本用户账单明细（含 pricing_snapshot——前端账单解释器数据源）。
///
/// **缺省 `scope=key`**：此前只按 user_id 过滤，合作商的员工 key 能翻到同一钱包下
/// 所有员工的请求——与 §6.1"员工只见自己"的门户缺省相悖。usage/breakdown/logs
/// 三个门户端点现在同一套 scope 语义。key 名一并回填：`scope=user` 时合作商
/// 要能看出"这笔是谁发的"，否则汇总视角没有意义。
pub async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let limit = q.limit.clamp(1, 200);
    let user_scope = q.scope.as_deref() == Some("user");
    let key_filter = if user_scope { None } else { Some(key.key_id) };
    let model = q.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
    let errors_only = q.errors_only == Some(true);
    let rows = sqlx::query!(
        r#"SELECT b.id, b.request_id, b.model_name, b.log_type, b.status, b.api_key_id,
                  COALESCE(k.name, '') AS "key_name!",
                  b.prompt_tokens, b.cached_tokens, b.completion_tokens, b.reasoning_tokens,
                  b.amount_micro, b.original_amount_micro, b.discount_micro,
                  b.pricing_snapshot, b.error_code, b.latency_ms, b.ttft_ms, b.is_stream, b.created_at
           FROM billing_records b
           LEFT JOIN api_keys k ON k.id = b.api_key_id
           WHERE b.user_id = $1
             AND ($2::bigint IS NULL OR b.id < $2)
             AND ($3::bigint IS NULL OR b.api_key_id = $3)
             AND ($4::text IS NULL OR b.model_name = $4)
             AND (NOT $5::boolean OR b.status <> 20)
           ORDER BY b.id DESC LIMIT $6"#,
        key.user_id,
        q.before,
        key_filter,
        model,
        errors_only,
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
                "request_id": r.request_id,
                "model": r.model_name,
                "log_type": r.log_type,
                "status": r.status,
                "api_key_id": r.api_key_id,
                "key_name": r.key_name,
                "usage": {
                    "prompt_tokens": r.prompt_tokens,
                    "cached_tokens": r.cached_tokens,
                    "completion_tokens": r.completion_tokens,
                    "reasoning_tokens": r.reasoning_tokens,
                },
                "amount_micro": r.amount_micro,
                "original_amount_micro": r.original_amount_micro,
                "discount_micro": r.discount_micro,
                "pricing_snapshot": r.pricing_snapshot,
                "error_code": r.error_code,
                "latency_ms": r.latency_ms,
                "ttft_ms": r.ttft_ms,
                "is_stream": r.is_stream,
                "created_at": r.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({
        "scope": if user_scope { "user" } else { "key" },
        "data": data,
        "next_before": next_before,
    })))
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
                  request_id, created_at
           FROM billing_events
           WHERE user_id = $1
             AND event_type IN ('recharge', 'adjust', 'refund', 'expire')
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
                "source": ledger_source(&r.actor, &tags),
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
fn ledger_source(actor: &str, tags: &[String]) -> &'static str {
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
    Json(req): Json<RedeemReq>,
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

    let claimed = okapi_store::admin::claim_redemption(&state.pg, code, key.user_id).await?;
    let Some(claimed) = claimed else {
        // 预查通过但翻转失败（竞争被抢/绑定他人）：回退 IP 计数
        if let Some((batch, ip)) = ip_charge {
            state.sched.redeem_ip_decr(batch, &ip).await;
        }
        return Err(AppError::new(StatusCode::NOT_FOUND, "redemption_invalid"));
    };

    let amount = okapi_domain::Money::from_micros(claimed.amount_micro);
    let balance_after = state.ledger.credit(key.user_id, amount).await?;
    okapi_ledger::pg::record_credit(
        &state.pg,
        key.user_id,
        amount,
        "adjust",
        "system:redeem",
        json!({
            "tags": ["redemption"],
            "code_id": claimed.code_id,
            "plan_code": claimed.plan_code,
        }),
    )
    .await?;

    // 套餐附带语义：加组失败/有效期失败不回滚入账（记日志走人工），核销主流程已成立
    if let Some(group) = &claimed.grant_group
        && let Err(err) = okapi_store::admin::add_user_group(&state.pg, key.user_id, group).await
    {
        tracing::error!(user_id = key.user_id, group, error = %err, "套餐加组失败（人工跟进）");
    }
    if let Some(days) = claimed.balance_valid_days {
        let expires = chrono::Utc::now() + chrono::Duration::days(i64::from(days));
        let result = sqlx::query!(
            r#"UPDATE users SET balance_expires_at = $2, updated_at = now() WHERE id = $1"#,
            key.user_id,
            expires
        )
        .execute(&state.pg)
        .await;
        if let Err(err) = result {
            tracing::error!(user_id = key.user_id, error = %err, "套餐余额有效期设置失败（人工跟进）");
        }
    }

    Ok(Json(json!({
        "amount_micro": claimed.amount_micro,
        "balance_after_micro": balance_after.as_micros(),
        "plan_code": claimed.plan_code,
        "granted_group": claimed.grant_group,
        "balance_valid_days": claimed.balance_valid_days,
    })))
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

/// GET /api/me/keys：本用户全部 key 及累计分账（合作商查员工用量）。
pub async fn keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let rows = sqlx::query!(
        r#"
        SELECT id, name, key_prefix, status, used_micro, rpm_limit, tpm_limit, rpd_limit,
               daily_token_limit, max_concurrency, model_allowlist, group_override, ip_allowlist,
               expires_at, last_used_at, created_at
        FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL ORDER BY id
        "#,
        key.user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    // CH 分账（可用时补充按 key 聚合；不可用时仅返回 PG used_micro 累计列）
    let mut ch_usage: Vec<Value> = Vec::new();
    if let Some(ch) = &state.ch {
        let ids: Vec<String> = rows.iter().map(|r| r.id.to_string()).collect();
        if !ids.is_empty() {
            let sql = format!(
                "SELECT api_key_id, sumMerge(amount) AS amount_micro, countMerge(requests) AS requests \
                 FROM mv_apikey_day WHERE api_key_id IN ({}) GROUP BY api_key_id",
                ids.join(",")
            );
            ch_usage = ch.query_json_each_row(&sql).await.unwrap_or_default();
        }
    }

    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let agg = ch_usage.iter().find(|u| ch_i64(u, "api_key_id") == r.id);
            json!({
                "id": r.id,
                "name": r.name,
                "key_prefix": r.key_prefix,
                "status": r.status,
                "used_micro": r.used_micro,
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
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

/// 自助面可改字段：收窄自己这把 key（名字 / 状态 / 过期 / 白名单），外加**在可选集合内**
/// 换分组——分组是计价锚点，但可选集合由管理员通过 `self_select` 与用户分组划定，
/// 用户只能在划定的档位里挑，不构成自行改价。限额仍只有管理面可写。
#[derive(Deserialize)]
pub struct PatchKeyReq {
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
    /// 档位：字符串 = 选定分组（须在 /api/me/groups 可选集合内）；null = 跟随用户分组。
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

/// PATCH /api/me/keys/{id}：改自己 key 的名称/启停/过期/模型白名单。
pub async fn patch_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<PatchKeyReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    if let Some(status) = req.status
        && !matches!(status, 1 | 2)
    {
        return Err(AppError::bad_request().with_param("status"));
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
pub async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
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
    let reward_sum = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(delta_micro), 0)::bigint AS "s!"
           FROM billing_events WHERE user_id = $1 AND actor = 'system:aff'"#,
        key.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    Ok(Json(json!({
        "aff_code": code,
        "invitees": invitees,
        "reward_sum_micro": reward_sum,
    })))
}
