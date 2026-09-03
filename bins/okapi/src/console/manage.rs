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

/// 渠道池列表：带渠道数与引用数，前端据此提示"被 N 个分组引用，先解绑"。
pub async fn list_pools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::CHANNEL_READ).await?;
    let rows = okapi_store::listing::list_pools(&state.pg).await?;
    Ok(Json(json!({
        "data": rows.iter().map(|r| json!({
            "pool_code": r.pool_code,
            "description": r.description,
            "routing_strategy": r.routing_strategy,
            "fallback_pool_code": r.fallback_pool_code,
            "builtin": r.pool_code == okapi_store::channels::DEFAULT_POOL,
            "channel_count": r.channel_count,
            "group_count": r.group_count,
            "key_count": r.key_count,
            "fallback_ref_count": r.fallback_ref_count,
        })).collect::<Vec<_>>()
    })))
}

/// 池详情：成员渠道（含覆盖）、能服务的模型并集、引用它的分组。
/// 分组抽屉据此把"分组 → 池 → 渠道 → 模型"三跳在一处展示，不必跨三个页面拼。
pub async fn pool_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::CHANNEL_READ).await?;
    let detail = okapi_store::listing::pool_detail(&state.pg, &code)
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!(detail)))
}

// ---- 路由诊断（"为什么这个请求没有候选"）----

#[derive(Deserialize)]
pub struct DiagnoseQuery {
    pub model: String,
    /// 按分组诊断（分组的 pool_code 决定可见范围）；缺省 = default 分组的池（即 default 池）。
    #[serde(default)]
    pub group: Option<String>,
    /// 直接钉住某池（模拟令牌 pool_override，优先于分组的池）。
    #[serde(default)]
    pub pool: Option<String>,
}

/// key 静态淘汰原因（与生产过滤 `ck.status = 1 AND cooldown 过期 AND subset 允许` 对应）。
fn key_reason(status: i16, cooling: bool, subset_ok: bool) -> Option<&'static str> {
    match status {
        2 => Some("key_cooling"),
        3 => Some("key_rate_limited"),
        4 => Some("key_quota_exhausted"),
        5 => Some("key_banned"),
        6 => Some("key_invalid"),
        _ if cooling => Some("key_cooling"),
        _ if !subset_ok => Some("model_subset_mismatch"),
        _ => None,
    }
}

/// 路由诊断：给定 模型 ×（分组 | 池），逐环回答"令牌→分组→池→渠道→key"
/// 每一跳的解析结果与淘汰原因。竞品共同的配置故障形态是"链路断在中间看不见"
/// （new-api FAQ '无可用渠道'三连查），本端点把三连查合成一次调用。
///
/// 幸存者集合直接复用生产查询 `candidates_for_model`——诊断口径与真实调度
/// 永不漂移；本函数只负责给"没进幸存者集合"的渠道/key 找出原因。
// 逐环线性拼装报告，拆分反而打断"模型→范围→渠道→兜底"的叙事
#[allow(clippy::too_many_lines)]
pub async fn diagnose_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DiagnoseQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::CHANNEL_READ).await?;
    let requested = q.model.trim();
    if requested.is_empty() {
        return Err(AppError::bad_request().with_param("model"));
    }

    // 环节 1：模型身份（别名感知、不过滤 status——"已停用"与"不存在"是两种病）
    let model_row = sqlx::query!(
        r#"
        SELECT m.model_name AS canonical, m.status, m.fallback_models,
               (p.model_id IS NOT NULL) AS "priced!"
        FROM models m
        LEFT JOIN model_pricing p ON p.model_id = m.id
        WHERE m.model_name = $1
           OR m.model_name = (
               SELECT target_model FROM model_aliases
               WHERE enabled AND (pattern = $1 OR $1 LIKE REPLACE(pattern, '*', '%'))
               ORDER BY (pattern = $1) DESC, priority DESC, pattern
               LIMIT 1
           )
        ORDER BY (m.model_name = $1) DESC
        LIMIT 1
        "#,
        requested
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    // 环节 2：分组 → 池（显式 pool 参数模拟令牌 pool_override，优先）；分组必有池，
    // 未给分组时按 default 池——与鉴权解析 COALESCE(override, group.pool, 'default') 同口径
    let group_row = if let Some(code) = q.group.as_deref().filter(|s| !s.is_empty()) {
        let row = sqlx::query!(
            r#"SELECT group_ratio::text AS "group_ratio!", pool_code
               FROM price_groups WHERE group_code = $1"#,
            code
        )
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        Some(row.ok_or_else(|| not_found().with_param("group"))?)
    } else {
        None
    };
    let explicit_pool = q.pool.as_deref().filter(|s| !s.is_empty());
    let (pool_code, pool_source) = match (explicit_pool, group_row.as_ref()) {
        (Some(p), _) => (p.to_owned(), "param"),
        (None, Some(g)) => (g.pool_code.clone(), "group"),
        (None, None) => (okapi_store::channels::DEFAULT_POOL.to_owned(), "default"),
    };
    let pool_row = sqlx::query!(
        r#"SELECT routing_strategy, fallback_pool_code FROM channel_pools WHERE pool_code = $1"#,
        pool_code
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| not_found().with_param("pool"))?;
    let routing_strategy = pool_row.routing_strategy;
    // 池链：主池 → 降级池（单跳）。候选与"为何被淘汰"都按链判定
    let mut pool_chain: Vec<String> = vec![pool_code.clone()];
    if let Some(fb) = pool_row.fallback_pool_code.filter(|fb| *fb != pool_code) {
        pool_chain.push(fb);
    }
    let chain_refs: Vec<&str> = pool_chain.iter().map(String::as_str).collect();

    // 模型不存在时 canonical 无从谈起，直接短路（渠道环节无意义）
    let Some(model_row) = model_row else {
        return Ok(Json(json!({
            "model": {"requested": requested, "canonical": null, "active": false,
                       "priced": false, "via_alias": false, "fallback_models": []},
            "scope": {"group_code": q.group, "group_ratio": group_row.map(|g| g.group_ratio),
                       "pool_code": pool_code, "pool_source": pool_source,
                       "pool_chain": pool_chain, "routing_strategy": routing_strategy},
            "channels": [], "candidates": 0,
            "verdict": "model_not_found", "fallbacks": []
        })));
    };
    let canonical = model_row.canonical;
    let active = model_row.status == 1;
    let fallback_chain: Vec<String> =
        serde_json::from_value(model_row.fallback_models).unwrap_or_default();

    // 环节 3：渠道与 key 全集（不过滤）+ 生产口径的幸存者集合
    let channels = okapi_store::channels::diagnose_channels(&state.pg, &canonical).await?;
    let survivors: std::collections::HashSet<i64> = okapi_store::channels::candidates_for_model(
        &state.pg,
        &canonical,
        &chain_refs,
        state.master_key.as_deref(),
    )
    .await
    .map(|v| v.iter().map(|c| c.channel_key_id).collect())
    .unwrap_or_default();

    let now = chrono::Utc::now();
    let channel_reports: Vec<Value> = channels
        .iter()
        .map(|ch| {
            let excluded = if ch.status != 1 {
                Some("channel_disabled")
            } else if ch.pools.is_empty() {
                // 孤儿：不在任何池里，对谁都不可达——这是"渠道只服务它所在的池"的直接后果
                Some("orphan_channel")
            } else if !ch.pools.iter().any(|c| pool_chain.contains(c)) {
                Some("not_in_pool")
            } else {
                None
            };
            // 经降级池才可见的渠道单独标注：正常时它不接流量
            let via_fallback =
                excluded.is_none() && !ch.pools.contains(&pool_code) && pool_chain.len() > 1;
            let keys: Vec<Value> = ch
                .keys
                .iter()
                .map(|k| {
                    let cooling = k.cooldown_until.is_some_and(|t| t > now);
                    let reason = key_reason(k.status, cooling, k.subset_ok);
                    json!({
                        "key_id": k.key_id,
                        "status": k.status,
                        "cooldown_until": k.cooldown_until,
                        "weight": k.weight,
                        "ok": survivors.contains(&k.key_id),
                        "reason": reason,
                        "caps": {"rpm": k.rpm_limit, "daily_spend_micro": k.daily_spend_cap_micro,
                                  "concurrency": k.max_concurrency},
                    })
                })
                .collect();
            json!({
                "channel_id": ch.channel_id, "name": &ch.name, "provider": &ch.provider,
                "status": ch.status, "priority": ch.priority, "pools": &ch.pools,
                "via_fallback": via_fallback,
                "excluded": excluded, "keys": keys,
            })
        })
        .collect();
    let candidates = survivors.len();

    // 环节 4：降级链逐环可投性预览（与网关 fallback_billing 同判据：存在且启用 + 有价 + 有候选）
    let mut fallbacks: Vec<Value> = Vec::new();
    for fb in &fallback_chain {
        let resolved = okapi_store::channels::resolve_model(&state.pg, fb).await?;
        let entry = if let Some(m) = resolved {
            let priced = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                       SELECT 1 FROM model_pricing p JOIN models m ON m.id = p.model_id
                       WHERE m.model_name = $1
                   ) AS "e!""#,
                m.canonical
            )
            .fetch_one(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
            let fb_candidates = okapi_store::channels::candidates_for_model(
                &state.pg,
                &m.canonical,
                &chain_refs,
                state.master_key.as_deref(),
            )
            .await
            .map_or(0, |v| v.len());
            let reason = if !priced {
                Some("unpriced")
            } else if fb_candidates == 0 {
                Some("no_available_channel")
            } else {
                None
            };
            json!({"model": m.canonical, "viable": priced && fb_candidates > 0,
                    "candidates": fb_candidates, "reason": reason})
        } else {
            json!({"model": fb, "viable": false, "candidates": 0, "reason": "missing_or_disabled"})
        };
        fallbacks.push(entry);
    }

    let verdict = if !active {
        "model_disabled"
    } else if !model_row.priced {
        "model_unpriced"
    } else if channels.is_empty() {
        "no_channel_serves_model"
    } else if candidates == 0 {
        "no_available_channel"
    } else {
        "ok"
    };

    Ok(Json(json!({
        "model": {"requested": requested, "canonical": canonical, "active": active,
                   "priced": model_row.priced, "via_alias": canonical != requested,
                   "fallback_models": fallback_chain},
        "scope": {"group_code": q.group, "group_ratio": group_row.map(|g| g.group_ratio),
                   "pool_code": pool_code, "pool_source": pool_source,
                   "pool_chain": pool_chain, "routing_strategy": routing_strategy},
        "channels": channel_reports,
        "candidates": candidates,
        "verdict": verdict,
        "fallbacks": fallbacks,
    })))
}

pub async fn delete_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::CHANNEL_WRITE).await?;
    if !mutate::delete_channel_pool(&state.pg, &code).await? {
        return Err(not_found());
    }
    state.invalidate_routing_caches();
    audit(&state, &actor, "channel.delete_pool", &code, json!({})).await;
    Ok(Json(json!({ "ok": true })))
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
