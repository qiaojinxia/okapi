//! 管理面 CRUD 补全（IMPLEMENTATION §11.6 接口面清单）。
//!
//! 与 `admin` 模块分工：`admin` 承载创建、单点动作与既有列表；本模块补齐资源管理闭环
//! ——**读列表、改、删、批量**，使管理后台可完整运维而不必直连数据库。
//!
//! 统一约定：
//! - 读走 `*_READ` 权限点，写沿用既有 `*_WRITE`；渠道读写继承 own/all 属主范围；
//! - 分页统一 `?limit=&offset=`，上限由 store 层 `clamp_page` 钳制；
//! - 不存在 → 404；被引用 → 409（error_code 指明占用方，前端渲染文案）；
//! - 写操作全量落 audit_logs；定价类变更回 `requires_publish` 提示需发布新 epoch。

use super::admin::{audit, ensure_channel_owner, guard, guard_scoped, guard_super_admin};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_store::auth::PermScope;
use okapi_store::mutate::{self, UserAction};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct PageQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// 关键词（令牌列表：name/username 模糊）。
    #[serde(default)]
    pub q: Option<String>,
    /// 过滤：用户 id（令牌列表）。
    #[serde(default)]
    pub user_id: Option<i64>,
    /// 过滤：批次（兑换码列表）。
    #[serde(default)]
    pub batch: Option<Uuid>,
    /// 过滤：状态。
    #[serde(default)]
    pub status: Option<i16>,
}

const fn default_limit() -> i64 {
    50
}

fn not_found() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
}

// ---- 列表（模型配置 / 分组 / 套餐 / 兑换码 / 令牌 / 设置 / 权限点）----

pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_READ).await?;
    let data = okapi_store::listing::list_models(&state.pg).await?;
    Ok(Json(json!({ "data": data })))
}

pub async fn list_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_READ).await?;
    let data = okapi_store::listing::list_groups(&state.pg).await?;
    Ok(Json(json!({ "data": data })))
}

pub async fn list_plans(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_READ).await?;
    let data = okapi_store::listing::list_plans(&state.pg).await?;
    Ok(Json(json!({ "data": data })))
}

pub async fn list_redemptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_READ).await?;
    let page =
        okapi_store::listing::list_redemptions(&state.pg, q.batch, q.status, q.limit, q.offset)
            .await?;
    Ok(Json(json!({ "data": page.data, "total": page.total })))
}

pub async fn list_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::USER_READ).await?;
    let page = okapi_store::listing::list_api_keys(
        &state.pg,
        q.user_id,
        q.q.as_deref(),
        q.limit,
        q.offset,
    )
    .await?;
    Ok(Json(json!({ "data": page.data, "total": page.total })))
}

/// 权限点清单（前端角色编辑器的选项来源，避免前端硬编码与后端漂移）。
pub async fn list_permissions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::ROLE_MANAGE).await?;
    Ok(Json(json!({ "data": permissions::ALL })))
}

/// 敏感设置键判定：列表接口只回"是否已配置"，明文永不出列表。
fn is_secret_key(key: &str) -> bool {
    const NEEDLES: [&str; 6] = [
        "secret",
        "key",
        "token",
        "password",
        "webhook",
        "credential",
    ];
    let lower = key.to_ascii_lowercase();
    NEEDLES.iter().any(|n| lower.contains(n))
}

pub async fn list_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::SETTINGS_READ).await?;
    let rows = okapi_store::listing::list_settings(&state.pg).await?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let secret = is_secret_key(&r.key);
            json!({
                "key": r.key,
                "value": if secret { Value::Null } else { r.value.clone() },
                "is_secret": secret,
                "configured": !r.value.is_null(),
                "updated_by": r.updated_by,
                "updated_at": r.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

// ---- 渠道（供应商接入）：更新 / 删除 / 批量 / 复制 ----

#[derive(Deserialize)]
pub struct BatchChannelReq {
    pub ids: Vec<i64>,
    /// enable | disable | delete
    pub action: String,
}

/// 批量启停/删除渠道（对齐 new-api 批量操作）。
/// own 范围拒绝批量：逐条属主校验会放大误操作面，own 角色请用单条端点。
pub async fn batch_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BatchChannelReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    if scope != PermScope::All {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("batch_requires_all_scope"),
        );
    }
    if req.ids.is_empty() {
        return Err(AppError::bad_request().with_param("ids"));
    }
    let affected = match req.action.as_str() {
        "enable" => mutate::batch_set_channel_status(&state.pg, &req.ids, 1).await?,
        "disable" => mutate::batch_set_channel_status(&state.pg, &req.ids, 2).await?,
        "delete" => mutate::batch_delete_channels(&state.pg, &req.ids).await?,
        _ => return Err(AppError::bad_request().with_param("action")),
    };
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.batch",
        &req.action,
        json!({"ids": req.ids, "affected": affected}),
    )
    .await;
    Ok(Json(json!({ "affected": affected })))
}

#[derive(Deserialize)]
pub struct DuplicateReq {
    pub name: String,
}

pub async fn duplicate_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<DuplicateReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    let name = req.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request().with_param("name"));
    }
    let new_id = mutate::duplicate_channel(&state.pg, id, name)
        .await?
        .ok_or_else(not_found)?;
    audit(
        &state,
        &actor,
        "channel.duplicate",
        &id.to_string(),
        json!({"new_id": new_id}),
    )
    .await;
    Ok(Json(json!({ "id": new_id, "status": 2 })))
}

// ---- 定价配置：删除 ----

pub async fn delete_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if !mutate::delete_model(&state.pg, &model).await? {
        return Err(not_found());
    }
    state.invalidate_routing_caches();
    audit(&state, &actor, "pricing.delete_model", &model, json!({})).await;
    Ok(Json(json!({ "ok": true, "requires_publish": true })))
}

pub async fn delete_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if !mutate::delete_price_group(&state.pg, &code).await? {
        return Err(not_found());
    }
    state.invalidate_routing_caches();
    audit(&state, &actor, "pricing.delete_group", &code, json!({})).await;
    Ok(Json(json!({ "ok": true, "requires_publish": true })))
}

pub async fn delete_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if !mutate::delete_plan(&state.pg, &code).await? {
        return Err(not_found());
    }
    audit(&state, &actor, "plan.delete", &code, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn disable_redemption_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(batch): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let affected = mutate::disable_redemption_batch(&state.pg, batch).await?;
    audit(
        &state,
        &actor,
        "redemption.disable_batch",
        &batch.to_string(),
        json!({"affected": affected}),
    )
    .await;
    Ok(Json(json!({ "affected": affected })))
}

pub async fn delete_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    // 角色变更强制 super_admin（与 create_role 同策略，防提权链）
    let actor = guard_super_admin(&state, &headers).await?;
    if !mutate::delete_role(&state.pg, &code).await? {
        return Err(not_found());
    }
    audit(&state, &actor, "role.delete", &code, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct RuleToggleReq {
    pub enabled: bool,
}

/// 规则启停（活动上下线，不删配置便于复用）。
pub async fn toggle_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Json(req): Json<RuleToggleReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if !mutate::set_pricing_rule_enabled(&state.pg, &code, req.enabled).await? {
        return Err(not_found());
    }
    audit(
        &state,
        &actor,
        "pricing.toggle_rule",
        &code,
        json!({"enabled": req.enabled}),
    )
    .await;
    Ok(Json(json!({ "ok": true, "requires_publish": true })))
}

// ---- 用户与令牌管理 ----

#[derive(Deserialize)]
pub struct ManageUserReq {
    /// ban | unban | promote | demote | delete
    pub action: String,
}

/// 用户管理统一端点（吸收 new-api `POST /api/user/manage` 形状）。
///
/// 安全约束：不可作用于自己；super_admin 不可被任何动作作用（即便调用方也是
/// super_admin——避免互踢导致站点失去最高权限）。封禁/删除连带吊销令牌并刷新鉴权缓存。
pub async fn manage_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<ManageUserReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_MANAGE).await?;
    let Some(action) = UserAction::parse(&req.action) else {
        return Err(AppError::bad_request().with_param("action"));
    };
    if id == actor.user_id {
        return Err(AppError::bad_request().with_param("self_target"));
    }
    let target_role = sqlx::query_scalar!(
        r#"SELECT role FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(not_found)?;
    if target_role >= 100 {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("super_admin_protected"),
        );
    }
    if !mutate::manage_user(&state.pg, id, action).await? {
        return Err(not_found());
    }
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "user.manage",
        &id.to_string(),
        json!({"action": req.action}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::is_secret_key;

    #[test]
    fn secret_keys_are_detected_case_insensitively() {
        for key in [
            "epay_key",
            "STRIPE_SECRET",
            "notify_webhook_url",
            "oauth.github.client_secret",
            "turnstile_secret_key",
            "channel_credential",
        ] {
            assert!(is_secret_key(key), "{key} 必须被识别为敏感键");
        }
        for key in ["site_name", "mcp_write_enabled", "retention_days"] {
            assert!(!is_secret_key(key), "{key} 不应被误判");
        }
    }
}
