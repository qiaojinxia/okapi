//! 管理 API 处理器（M2 第一批）。
//! 鉴权：Bearer api key → users.role >= 10；写操作全量 audit_logs 留痕。

use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_domain::Money;
use okapi_store::AuthedKey;
use okapi_store::auth::PermScope;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

/// 权限点守卫（IMPLEMENTATION §6.2）：403 时 param 携带缺失的权限点，便于排障。
async fn guard(
    state: &AppState,
    headers: &HeaderMap,
    permission: &str,
) -> Result<Arc<AuthedKey>, AppError> {
    let key = authenticate(state, headers).await?;
    if !key.has_permission(permission) {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param(permission.to_owned()),
        );
    }
    Ok(key)
}

/// 带 own/all 范围的守卫（#6267）：Denied → 403；返回范围供 handler 做属主过滤。
async fn guard_scoped(
    state: &AppState,
    headers: &HeaderMap,
    base: &str,
) -> Result<(Arc<AuthedKey>, PermScope), AppError> {
    let key = authenticate(state, headers).await?;
    match key.permission_scope(base) {
        PermScope::Denied => Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param(base.to_owned()),
        ),
        scope => Ok((key, scope)),
    }
}

/// own 范围下的渠道属主校验。
async fn ensure_channel_owner(
    state: &AppState,
    channel_id: i64,
    actor: &AuthedKey,
    scope: PermScope,
) -> Result<(), AppError> {
    if scope == PermScope::All {
        return Ok(());
    }
    let owner = okapi_store::admin::channel_owner(&state.pg, channel_id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    if owner != Some(actor.user_id) {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED).with_param("owner"),
        );
    }
    Ok(())
}

/// 角色管理强制 super_admin：防止"默认全权 admin"给自己发角色形成提权链。
async fn guard_super_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<AuthedKey>, AppError> {
    let key = guard(state, headers, permissions::ROLE_MANAGE).await?;
    if key.role < 100 {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("super_admin_required"),
        );
    }
    Ok(key)
}

async fn audit(state: &AppState, actor: &AuthedKey, action: &str, target: &str, detail: Value) {
    if let Err(err) = okapi_store::admin::record_audit(
        &state.pg,
        &format!("admin:{}", actor.user_id),
        action,
        target,
        detail,
    )
    .await
    {
        tracing::error!(error = %err, action, "审计写入失败");
    }
}

// ---- 渠道 ----

#[derive(Deserialize)]
pub struct CreateChannelReq {
    pub name: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    pub api_base: String,
    pub credential: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub trust_upstream_usage: bool,
    #[serde(default)]
    pub max_concurrency: Option<i32>,
    /// 可见组绑定（空 = 不绑定，宽松模式下全可见）。
    #[serde(default)]
    pub groups: Vec<String>,
    /// 渠道高级设置（channels.settings 对象整体；已注册键见 docs/database.md：
    /// thinking_to_content / bill_by_response_model / strip_request_fields / pass_paths）。
    #[serde(default)]
    pub settings: Option<Value>,
}

fn default_provider() -> String {
    "openai".to_owned()
}

pub async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateChannelReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, _) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    super::ssrf::validate_api_base(&state, &req.api_base).await?;
    let models: Vec<&str> = req.models.iter().map(String::as_str).collect();
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel(
        &state.pg,
        &req.name,
        &req.provider,
        &req.api_base,
        &req.credential,
        &models,
        req.trust_upstream_usage,
    )
    .await?;
    if let Some(settings) = &req.settings {
        if !settings.is_object() {
            return Err(AppError::bad_request().with_param("settings"));
        }
        sqlx::query!(
            "UPDATE channels SET settings = $2 WHERE id = $1",
            channel_id,
            settings
        )
        .execute(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    if req.priority != 0 {
        sqlx::query!(
            "UPDATE channels SET priority = $2 WHERE id = $1",
            channel_id,
            req.priority
        )
        .execute(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    if let Some(cap) = req.max_concurrency {
        sqlx::query!(
            "UPDATE channel_keys SET max_concurrency = $2 WHERE id = $1",
            channel_key_id,
            cap
        )
        .execute(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    // 属主传播（#6267）：创建人即属主，own 范围据此过滤
    okapi_store::admin::set_channel_owner(&state.pg, channel_id, actor.user_id).await?;
    if !req.groups.is_empty() {
        okapi_store::admin::set_channel_groups(&state.pg, channel_id, &req.groups).await?;
    }
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.create",
        &channel_id.to_string(),
        json!({ "name": req.name, "provider": req.provider, "models": req.models }),
    )
    .await;
    Ok(Json(
        json!({ "channel_id": channel_id, "channel_key_id": channel_key_id }),
    ))
}

pub async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let owner_filter = match scope {
        PermScope::Own => Some(actor.user_id),
        PermScope::All | PermScope::Denied => None,
    };
    let channels = okapi_store::admin::list_channels(&state.pg, owner_filter).await?;
    let keys = okapi_store::admin::list_channel_keys(&state.pg).await?;
    let data: Vec<Value> = channels
        .into_iter()
        .map(|c| {
            let keys: Vec<&okapi_store::admin::ChannelKeyRow> =
                keys.iter().filter(|k| k.channel_id == c.id).collect();
            json!({
                "id": c.id, "name": c.name, "provider": c.provider,
                "api_base": c.api_base, "status": c.status, "priority": c.priority,
                "models": c.models, "trust_upstream_usage": c.trust_upstream_usage,
                "keys": keys,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

#[derive(Deserialize)]
pub struct SetStatusReq {
    pub status: i16,
}

pub async fn set_channel_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<SetStatusReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    if !matches!(req.status, 1 | 2) {
        return Err(AppError::bad_request());
    }
    ensure_channel_owner(&state, id, &actor, scope).await?;
    let hit = okapi_store::admin::set_channel_status(&state.pg, id, req.status).await?;
    if !hit {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.set_status",
        &id.to_string(),
        json!({ "status": req.status }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetGroupsReq {
    pub groups: Vec<String>,
}

/// 覆盖式设置渠道可见组（§6.3 可见性矩阵）。
pub async fn set_channel_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<SetGroupsReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    okapi_store::admin::set_channel_groups(&state.pg, id, &req.groups).await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.set_groups",
        &id.to_string(),
        json!({ "groups": req.groups }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 模型与定价 ----

#[derive(Deserialize)]
pub struct UpsertGroupReq {
    pub group_code: String,
    /// 分组倍率（十进制字符串）。
    pub group_ratio: String,
    #[serde(default)]
    pub description: String,
}

/// 定价分组 upsert（改倍率后需 publish 生效）。
pub async fn upsert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertGroupReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if req.group_ratio.parse::<okapi_pricing::RatioFp>().is_err() {
        return Err(AppError::bad_request().with_param("group_ratio"));
    }
    okapi_store::admin::upsert_price_group(
        &state.pg,
        &req.group_code,
        &req.group_ratio,
        &req.description,
    )
    .await?;
    audit(
        &state,
        &actor,
        "pricing.upsert_group",
        &req.group_code,
        json!({ "group_ratio": req.group_ratio }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct UserGroupsReq {
    /// [(组, 优先级)]：定价取最高优先级组，可见性取并集。
    pub groups: Vec<UserGroupItem>,
}

#[derive(Deserialize)]
pub struct UserGroupItem {
    pub group_code: String,
    #[serde(default)]
    pub priority: i32,
}

/// 覆盖式设置用户分组（§6.3）。
pub async fn set_user_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(req): Json<UserGroupsReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_MANAGE).await?;
    let groups: Vec<(String, i32)> = req
        .groups
        .iter()
        .map(|g| (g.group_code.clone(), g.priority))
        .collect();
    okapi_store::admin::set_user_groups(&state.pg, user_id, &groups).await?;
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "user.set_groups",
        &user_id.to_string(),
        json!({ "groups": groups.iter().map(|(c, p)| json!({"code": c, "priority": p})).collect::<Vec<_>>() }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetSettingReq {
    pub key: String,
    pub value: Value,
}

/// 全局设置写入（如 strict_group_isolation）。
pub async fn set_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetSettingReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    okapi_store::admin::set_setting(&state.pg, &req.key, &req.value, actor.user_id).await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "settings.set",
        &req.key,
        json!({ "value": req.value }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// GET /admin/settings/{key}：单键读取（配置页回显；写权限同门槛防低权限窥探敏感配置）。
pub async fn get_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = $1"#, key)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({ "key": key, "value": value })))
}

#[derive(Deserialize)]
pub struct LeaderboardQuery {
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// GET /admin/leaderboard：用户消费排行（#1790-11，CH mv_user_day 聚合 + PG 补用户名）。
pub async fn leaderboard(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<LeaderboardQuery>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::BILLING_READ).await?;
    let Some(ch) = state.ch.as_ref() else {
        return Err(AppError::new(
            StatusCode::NOT_IMPLEMENTED,
            codes::STATS_DISABLED,
        ));
    };
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let sql = format!(
        "SELECT user_id, countMerge(requests) AS requests, \
                sumMerge(tokens) AS tokens, sumMerge(amount) AS amount_micro \
         FROM mv_user_day WHERE day >= today() - {days} \
         GROUP BY user_id ORDER BY amount_micro DESC LIMIT {limit}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    // 用户名补齐（PG 点查；榜单 ≤100 行）
    let ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| {
            r.get("user_id")
                .and_then(|v| v.as_str().map_or_else(|| v.as_i64(), |s| s.parse().ok()))
        })
        .collect();
    let names = sqlx::query!(r#"SELECT id, username FROM users WHERE id = ANY($1)"#, &ids)
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let name_of: std::collections::HashMap<i64, String> =
        names.into_iter().map(|r| (r.id, r.username)).collect();

    let data: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            let uid = r
                .get("user_id")
                .and_then(|v| v.as_str().map_or_else(|| v.as_i64(), |s| s.parse().ok()))
                .unwrap_or(0);
            if let Some(obj) = r.as_object_mut() {
                obj.insert(
                    "username".into(),
                    json!(name_of.get(&uid).cloned().unwrap_or_default()),
                );
            }
            r
        })
        .collect();
    Ok(Json(json!({ "days": days, "data": data })))
}

#[derive(Deserialize)]
pub struct UpsertModelReq {
    pub model_name: String,
    /// 倍率一律十进制字符串（精确入库，禁浮点）。
    pub model_ratio: String,
    #[serde(default = "default_one")]
    pub completion_ratio: String,
    #[serde(default = "default_one")]
    pub cache_ratio: String,
    /// 缓存写入倍率（Anthropic cache_creation；官方 1.25×@5m / 2.0×@1h，缺省 1=按常规输入计）。
    /// 兼容 new-api 键名 `create_cache_ratio`。
    #[serde(default = "default_one", alias = "create_cache_ratio")]
    pub cache_write_ratio: String,
    /// service_tier 档位倍率（如 {"flex":"0.5","priority":"2.0"}；
    /// None=不改动，空对象=清除，DESIGN §3-4.5）。
    #[serde(default)]
    pub tier_ratios: Option<serde_json::Map<String, Value>>,
}

fn default_one() -> String {
    "1".to_owned()
}

pub async fn upsert_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertModelReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    // 倍率字面量校验（复用定价域解析器，非法值 400）
    for literal in [
        &req.model_ratio,
        &req.completion_ratio,
        &req.cache_ratio,
        &req.cache_write_ratio,
    ] {
        if literal.parse::<okapi_pricing::RatioFp>().is_err() {
            return Err(AppError::bad_request().with_param("ratio"));
        }
    }
    let model_id = okapi_store::admin::upsert_model_ratio(
        &state.pg,
        &req.model_name,
        &req.model_ratio,
        &req.completion_ratio,
        &req.cache_ratio,
        &req.cache_write_ratio,
    )
    .await?;
    if let Some(tiers) = &req.tier_ratios {
        for v in tiers.values() {
            let ok = v
                .as_str()
                .is_some_and(|s| s.parse::<okapi_pricing::RatioFp>().is_ok());
            if !ok {
                return Err(AppError::bad_request().with_param("tier_ratios"));
            }
        }
        let value = if tiers.is_empty() {
            None
        } else {
            Some(Value::Object(tiers.clone()))
        };
        sqlx::query!(
            r#"UPDATE model_pricing SET tier_ratios = $2 WHERE model_id = $1"#,
            model_id,
            value
        )
        .execute(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "pricing.upsert_model",
        &req.model_name,
        json!({
            "model_ratio": req.model_ratio,
            "completion_ratio": req.completion_ratio,
            "cache_ratio": req.cache_ratio,
            "cache_write_ratio": req.cache_write_ratio,
        }),
    )
    .await;
    Ok(Json(json!({ "model_id": model_id })))
}

/// 发布定价 epoch：**发布前全量编译校验（fail-closed，DESIGN §3.3）**——
/// 配置编译不过即拒绝发布；snapshot 存发布时刻配置全量（历史/回滚/diff）。
pub async fn publish_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_PUBLISH).await?;

    let rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let source = crate::gateway::pricing_loader::build_source(&rows);
    if let Err(err) = okapi_pricing::book::compile(source) {
        tracing::warn!(error = %err, "定价配置编译失败，拒绝发布");
        return Err(AppError::bad_request().with_param(format!("compile: {err}")));
    }
    let snapshot = serde_json::to_value(&rows).map_err(|_| AppError::internal())?;

    let epoch = okapi_store::admin::publish_epoch(&state.pg, actor.user_id, &snapshot).await?;
    // 广播（best-effort；丢失由 gateway 30s 轮询兜底）
    if let Some(nats) = &state.nats {
        let _ = nats
            .publish("pricing.epoch", epoch.to_string().into())
            .await;
    }
    audit(
        &state,
        &actor,
        "pricing.publish",
        &epoch.to_string(),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "epoch": epoch })))
}

// ---- 用户入账 ----

#[derive(Deserialize)]
pub struct CreditReq {
    pub amount_micro: i64,
    #[serde(default)]
    pub reason: String,
}

pub async fn credit_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(req): Json<CreditReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    if req.amount_micro <= 0 {
        return Err(AppError::bad_request().with_param("amount_micro"));
    }
    let amount = Money::from_micros(req.amount_micro);
    let balance_after = state.ledger.credit(user_id, amount).await?;
    okapi_ledger::pg::record_credit(
        &state.pg,
        user_id,
        amount,
        "adjust",
        &format!("admin:{}", actor.user_id),
        json!({ "tags": ["manual_credit"], "reason": req.reason }),
    )
    .await?;
    audit(
        &state,
        &actor,
        "user.credit",
        &user_id.to_string(),
        json!({ "amount_micro": req.amount_micro, "reason": req.reason }),
    )
    .await;
    Ok(Json(
        json!({ "balance_after_micro": balance_after.as_micros() }),
    ))
}

#[derive(Deserialize)]
pub struct BalanceExpiryReq {
    /// RFC3339；null = 取消有效期（永不过期）。
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 设置/取消余额有效期（#1790-6）：到期由 worker 清零并事件留痕。
pub async fn set_balance_expiry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(req): Json<BalanceExpiryReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    let updated = sqlx::query!(
        r#"UPDATE users SET balance_expires_at = $2, updated_at = now()
           WHERE id = $1 AND deleted_at IS NULL"#,
        user_id,
        req.expires_at
    )
    .execute(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if updated.rows_affected() == 0 {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    audit(
        &state,
        &actor,
        "user.balance_expiry",
        &user_id.to_string(),
        json!({ "expires_at": req.expires_at }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 角色管理（仅 super_admin）----

#[derive(Deserialize)]
pub struct CreateRoleReq {
    pub role_code: String,
    pub display_name: String,
    /// 权限点集合（okapi_api::permissions 常量；`*` 为全权）。
    pub permissions: Vec<String>,
}

pub async fn create_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRoleReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard_super_admin(&state, &headers).await?;
    let permissions = json!(req.permissions);
    let role_id = okapi_store::admin::create_admin_role(
        &state.pg,
        &req.role_code,
        &req.display_name,
        &permissions,
    )
    .await?;
    audit(
        &state,
        &actor,
        "role.create",
        &req.role_code,
        json!({ "permissions": req.permissions }),
    )
    .await;
    Ok(Json(json!({ "admin_role_id": role_id })))
}

#[derive(Deserialize)]
pub struct AssignRoleReq {
    /// 平台角色（1/10/100）；None = 不变。
    #[serde(default)]
    pub role: Option<i16>,
    /// 自定义子角色 id；None = 解绑（admin 回到默认全权）。
    #[serde(default)]
    pub admin_role_id: Option<i64>,
}

pub async fn assign_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(req): Json<AssignRoleReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard_super_admin(&state, &headers).await?;
    if let Some(role) = req.role
        && !matches!(role, 1 | 10 | 100)
    {
        return Err(AppError::bad_request().with_param("role"));
    }
    let hit = okapi_store::admin::assign_user_role(&state.pg, user_id, req.role, req.admin_role_id)
        .await?;
    if !hit {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "role.assign",
        &user_id.to_string(),
        json!({ "role": req.role, "admin_role_id": req.admin_role_id }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 按日志退款 ----

#[derive(Deserialize)]
pub struct RefundReq {
    pub request_id: Uuid,
    #[serde(default)]
    pub reason: String,
}

/// 按日志退款（§5.3）：事件溯源冲销，账单/统计/余额三处口径自动一致；幂等。
pub async fn refund_by_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RefundReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::BILLING_REFUND).await?;
    let outcome = okapi_ledger::pg::admin_refund(
        &state.pg,
        req.request_id,
        &req.reason,
        &format!("admin:{}", actor.user_id),
    )
    .await
    .map_err(AppError::from)?;
    let Some(refund) = outcome else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("request_id"));
    };
    // Redis 热余额回补（PG 已提交；此处失败由对账检出并修复）
    let balance_after = state.ledger.credit(refund.user_id, refund.amount).await?;
    audit(
        &state,
        &actor,
        "billing.refund",
        &req.request_id.to_string(),
        json!({ "amount_micro": refund.amount.as_micros(), "reason": req.reason }),
    )
    .await;
    Ok(Json(json!({
        "user_id": refund.user_id,
        "refunded_micro": refund.amount.as_micros(),
        "balance_after_micro": balance_after.as_micros(),
    })))
}

// ---- 代客查看（#1790-2，强审计）----

pub async fn user_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_ASSIST).await?;
    let Some(user) = sqlx::query!(
        r#"SELECT id, username, role, status, balance_micro, price_multiplier::text AS "multiplier!"
           FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    let groups = sqlx::query!(
        r#"SELECT group_code, priority FROM user_groups WHERE user_id = $1 ORDER BY priority DESC"#,
        user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let keys = sqlx::query!(
        r#"SELECT id, name, key_prefix, status, used_micro FROM api_keys
           WHERE user_id = $1 AND deleted_at IS NULL ORDER BY id"#,
        user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let hot_balance = state.ledger.balance(user_id).await?;

    // 代客操作必须留痕（谁看了谁）
    audit(
        &state,
        &actor,
        "user.assist.view",
        &user_id.to_string(),
        json!({}),
    )
    .await;

    Ok(Json(json!({
        "user": {
            "id": user.id, "username": user.username, "role": user.role,
            "status": user.status, "balance_micro": user.balance_micro,
            "hot_balance_micro": hot_balance.as_micros(),
            "price_multiplier": user.multiplier,
        },
        "groups": groups.iter().map(|g| json!({"code": g.group_code, "priority": g.priority})).collect::<Vec<_>>(),
        "keys": keys.iter().map(|k| json!({
            "id": k.id, "name": k.name, "key_prefix": k.key_prefix,
            "status": k.status, "used_micro": k.used_micro,
        })).collect::<Vec<_>>(),
    })))
}

// ---- 缓存清理（#1790-7）----

#[derive(Deserialize)]
pub struct FlushReq {
    /// auth | pricebook。
    pub scope: String,
}

#[derive(Deserialize)]
pub struct UpsertPlanReq {
    pub plan_code: String,
    pub display_name: String,
    pub grant_micro: i64,
    /// 兑换后追加分组（须为已存在的 price_groups.group_code）。
    #[serde(default)]
    pub group_code: Option<String>,
    /// 兑换后设置余额有效期（天）。
    #[serde(default)]
    pub balance_valid_days: Option<i32>,
}

/// 建/改套餐（#1790-5；plan_code 幂等 upsert）。
pub async fn upsert_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertPlanReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    if req.grant_micro <= 0 || req.plan_code.trim().is_empty() {
        return Err(AppError::bad_request().with_param("plan"));
    }
    if req.balance_valid_days.is_some_and(|d| d <= 0) {
        return Err(AppError::bad_request().with_param("balance_valid_days"));
    }
    let id = okapi_store::admin::create_plan(
        &state.pg,
        req.plan_code.trim(),
        req.display_name.trim(),
        req.grant_micro,
        req.group_code.as_deref(),
        req.balance_valid_days,
    )
    .await?;
    audit(
        &state,
        &actor,
        "plan.upsert",
        &req.plan_code,
        json!({
            "grant_micro": req.grant_micro,
            "group_code": req.group_code,
            "balance_valid_days": req.balance_valid_days,
        }),
    )
    .await;
    Ok(Json(json!({ "plan_id": id })))
}

#[derive(Deserialize)]
pub struct CreateRedemptionsReq {
    pub count: u32,
    pub amount_micro: i64,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 绑套餐（核销金额取套餐 grant 覆盖面值，可附带加组/余额有效期，#1790-5）。
    #[serde(default)]
    pub plan_code: Option<String>,
    /// 限定核销用户（他人核销与不存在同响应）。
    #[serde(default)]
    pub bind_user_id: Option<i64>,
    /// 同批次单 IP 核销上限（依赖 CDN 头取 IP，直连无头不限）。
    #[serde(default)]
    pub max_per_ip: Option<i32>,
}

/// 批量生成兑换码（明文仅本次返回；核销走门户 /api/me/redeem）。
pub async fn create_redemptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRedemptionsReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    if req.amount_micro <= 0 || req.count == 0 || req.count > 1000 {
        return Err(AppError::bad_request().with_param("count_or_amount"));
    }
    let codes: Vec<String> = (0..req.count)
        .map(|_| {
            use rand::RngExt;
            use rand::distr::Alphanumeric;
            let body: String = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(24)
                .map(char::from)
                .collect();
            format!("okapi-{body}")
        })
        .collect();
    let batch_id = okapi_store::admin::create_redemption_codes(
        &state.pg,
        actor.user_id,
        req.amount_micro,
        &codes,
        req.expires_at,
        okapi_store::admin::RedemptionOptions {
            plan_code: req.plan_code.as_deref(),
            bind_user_id: req.bind_user_id,
            max_per_ip: req.max_per_ip,
        },
    )
    .await?
    .ok_or_else(|| AppError::bad_request().with_param("plan_code"))?;
    audit(
        &state,
        &actor,
        "redemption.create",
        &batch_id.to_string(),
        json!({
            "count": req.count, "amount_micro": req.amount_micro,
            "plan_code": req.plan_code, "bind_user_id": req.bind_user_id,
            "max_per_ip": req.max_per_ip,
        }),
    )
    .await;
    Ok(Json(json!({
        "batch_id": batch_id,
        // 明文仅生成时返回一次
        "codes": codes,
    })))
}

#[derive(Deserialize)]
pub struct ImportNewApiReq {
    #[serde(default)]
    pub model_ratio: serde_json::Map<String, Value>,
    #[serde(default)]
    pub completion_ratio: serde_json::Map<String, Value>,
    #[serde(default)]
    pub cache_ratio: serde_json::Map<String, Value>,
    /// new-api 缓存**写入**倍率（其键名为 create_cache_ratio）。
    #[serde(default, alias = "cache_write_ratio")]
    pub create_cache_ratio: serde_json::Map<String, Value>,
    /// new-api 按次价（USD）。
    #[serde(default)]
    pub model_price: serde_json::Map<String, Value>,
}

/// JSON 值 → 十进制字面量（数字经 ryu 最短表示 roundtrip，字符串原样）。
fn ratio_literal(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// new-api 官方 ratio JSON 一键导入（M3 验收项）：
/// model_ratio 为主表（completion/cache 缺省 1），model_price → per_call；
/// 非法值跳过并报告；导入后不自动发布 epoch（管理员 review 后手动 publish）。
pub async fn import_newapi_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ImportNewApiReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let mut imported: i64 = 0;
    let mut skipped: Vec<String> = Vec::new();

    for (model, v) in &req.model_ratio {
        let Some(model_ratio) = ratio_literal(v) else {
            skipped.push(model.clone());
            continue;
        };
        let completion = req
            .completion_ratio
            .get(model)
            .and_then(ratio_literal)
            .unwrap_or_else(|| "1".to_owned());
        let cache = req
            .cache_ratio
            .get(model)
            .and_then(ratio_literal)
            .unwrap_or_else(|| "1".to_owned());
        let cache_write = req
            .create_cache_ratio
            .get(model)
            .and_then(ratio_literal)
            .unwrap_or_else(|| "1".to_owned());
        // 复用定价域解析器做入库前校验（纯整数定点，禁浮点）
        if [&model_ratio, &completion, &cache, &cache_write]
            .iter()
            .any(|s| s.parse::<okapi_pricing::RatioFp>().is_err())
        {
            skipped.push(model.clone());
            continue;
        }
        okapi_store::admin::upsert_model_ratio(
            &state.pg,
            model,
            &model_ratio,
            &completion,
            &cache,
            &cache_write,
        )
        .await?;
        imported += 1;
    }

    for (model, v) in &req.model_price {
        let Some(literal) = ratio_literal(v) else {
            skipped.push(model.clone());
            continue;
        };
        // USD 十进制 → micro（scale=1e6 定点解析，恰为 micro-USD）
        let Ok(ratio) = literal.parse::<okapi_pricing::RatioFp>() else {
            skipped.push(model.clone());
            continue;
        };
        okapi_store::admin::upsert_model_per_call(&state.pg, model, ratio.as_scaled()).await?;
        imported += 1;
    }

    audit(
        &state,
        &actor,
        "pricing.import_newapi",
        "batch",
        json!({ "imported": imported, "skipped": skipped }),
    )
    .await;
    Ok(Json(
        json!({ "imported": imported, "skipped": skipped, "published": false }),
    ))
}

/// 渠道测活：按渠道协议发轻量探测（models 列表/根路径），返回可达性与延迟。
/// 语义：2xx = ok；401/403 = 可达但凭证问题（http_status 供管理员判断）；
/// 网络失败 = 不可达。own 范围管理员只能测自己名下渠道。
pub async fn test_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    let result = probe_channel(&state, channel_id).await?;
    audit(
        &state,
        &actor,
        "channel.test",
        &channel_id.to_string(),
        result.clone(),
    )
    .await;
    Ok(Json(result))
}

/// 渠道探测核心（REST 与 MCP channel_test 共用）。
pub(crate) async fn probe_channel(state: &AppState, channel_id: i64) -> Result<Value, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT c.provider, c.api_base, ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        WHERE c.id = $1 AND c.deleted_at IS NULL
        ORDER BY ck.id
        LIMIT 1
        "#,
        channel_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            okapi_api::codes::NO_AVAILABLE_CHANNEL,
        )
    })?;
    let credential =
        String::from_utf8(row.credential_ciphertext).map_err(|_| AppError::internal())?;
    let base = row.api_base.unwrap_or_default();
    let base = base.trim_end_matches('/');

    // 按协议选探测端点与凭证头
    let (url, auth_header, auth_value) = match row.provider.as_str() {
        "anthropic" => (format!("{base}/models"), "x-api-key".to_owned(), credential),
        "gemini" => (
            format!("{base}/models"),
            "x-goog-api-key".to_owned(),
            credential,
        ),
        "custom_pass" => (
            base.to_owned(),
            "authorization".to_owned(),
            format!("Bearer {credential}"),
        ),
        _ => (
            format!("{base}/models"),
            "authorization".to_owned(),
            format!("Bearer {credential}"),
        ),
    };

    let started = std::time::Instant::now();
    let outcome = state
        .pass
        .forward(okapi_providers::custom_pass::PassRequest {
            method: axum::http::Method::GET,
            url,
            auth_header,
            auth_value,
            content_type: None,
            body: bytes::Bytes::new(),
        })
        .await;
    let latency_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let result = match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { status, .. }) => {
            json!({"ok": true, "http_status": status, "latency_ms": latency_ms})
        }
        Ok(okapi_providers::custom_pass::PassResponse::ErrStatus { status, .. }) => {
            json!({"ok": false, "http_status": status, "latency_ms": latency_ms})
        }
        Err(err) => {
            json!({"ok": false, "error_code": err.error_code(), "latency_ms": latency_ms})
        }
    };
    Ok(result)
}

/// 上游模型列表响应大小上限（Sub2API 0.1.180 吸收，防超大响应耗尽内存）。
const FETCH_MODELS_MAX_BYTES: usize = 8 * 1024 * 1024;

/// 渠道上游模型发现（§11.3，new-api #6184 吸收）：拉上游 /models 返回 id 列表，
/// 由管理员确认后经既有渠道更新通道写入（不自动改配置）。
pub async fn fetch_channel_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    let row = sqlx::query!(
        r#"
        SELECT c.provider, c.api_base, ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        WHERE c.id = $1 AND c.deleted_at IS NULL
        ORDER BY ck.id
        LIMIT 1
        "#,
        channel_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            okapi_api::codes::NO_AVAILABLE_CHANNEL,
        )
    })?;
    let credential =
        String::from_utf8(row.credential_ciphertext).map_err(|_| AppError::internal())?;
    let base = row.api_base.unwrap_or_default();
    let base = base.trim_end_matches('/');
    let (url, auth_header, auth_value) = match row.provider.as_str() {
        "anthropic" => (format!("{base}/models"), "x-api-key".to_owned(), credential),
        "gemini" => (
            format!("{base}/models"),
            "x-goog-api-key".to_owned(),
            credential,
        ),
        _ => (
            format!("{base}/models"),
            "authorization".to_owned(),
            format!("Bearer {credential}"),
        ),
    };

    let outcome = state
        .pass
        .forward(okapi_providers::custom_pass::PassRequest {
            method: axum::http::Method::GET,
            url,
            auth_header,
            auth_value,
            content_type: None,
            body: bytes::Bytes::new(),
        })
        .await;
    let body = match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { mut stream, .. }) => {
            use futures::StreamExt as _;
            let mut buf: Vec<u8> = Vec::new();
            while let Some(Ok(chunk)) = stream.next().await {
                if buf.len() + chunk.len() > FETCH_MODELS_MAX_BYTES {
                    return Err(AppError::bad_request().with_param("upstream_models_too_large"));
                }
                buf.extend_from_slice(&chunk);
            }
            buf
        }
        Ok(okapi_providers::custom_pass::PassResponse::ErrStatus { status, .. }) => {
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                    .with_param(format!("status_{status}")),
            );
        }
        Err(err) => {
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                    .with_param(err.error_code()),
            );
        }
    };
    let parsed: Value = serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request().with_param("upstream_models_not_json"))?;
    // openai/anthropic: {data:[{id}]}；gemini: {models:[{name: "models/x"}]}
    let mut models: Vec<String> = Vec::new();
    for item in parsed
        .get("data")
        .or_else(|| parsed.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            models.push(id.to_owned());
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            models.push(name.strip_prefix("models/").unwrap_or(name).to_owned());
        }
    }
    models.sort();
    models.dedup();
    Ok(Json(json!({ "channel_id": channel_id, "models": models })))
}

/// 缓存清理。auth 走 Redis 版本键（跨进程立即生效）；routing/pricebook 为进程内
/// （单机 all 形态全局生效；多副本靠 TTL 收敛，NATS 广播 M2 relay 批接入）。
pub async fn cache_flush(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FlushReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::CACHE_FLUSH).await?;
    match req.scope.as_str() {
        "auth" => state.sched.auth_flush().await,
        "routing" => state.invalidate_routing_caches(),
        "pricebook" => {
            let book = crate::gateway::pricing_loader::load_pricebook(&state.pg)
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "pricebook 重载失败");
                    AppError::internal()
                })?;
            state.pricebook.replace(book);
        }
        _ => return Err(AppError::bad_request().with_param("scope")),
    }
    audit(&state, &actor, "cache.flush", &req.scope, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 对账 ----

#[derive(Deserialize)]
pub struct ReconQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    1000
}

pub async fn reconciliation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReconQuery>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::BILLING_READ).await?;
    let drifts = crate::worker::reconcile_balances(&state.pg, &state.ledger, q.limit)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "对账查询失败");
            AppError::internal()
        })?;
    let data: Vec<Value> = drifts
        .iter()
        .map(|d| {
            json!({
                "user_id": d.user_id,
                "events_sum_micro": d.events_sum_micro,
                "redis_effective_micro": d.redis_effective_micro,
                "pg_snapshot_micro": d.pg_snapshot_micro,
            })
        })
        .collect();
    Ok(Json(json!({ "drift_count": data.len(), "drifts": data })))
}
