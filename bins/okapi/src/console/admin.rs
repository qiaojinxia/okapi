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
pub(super) async fn guard(
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
pub(super) async fn guard_scoped(
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
pub(super) async fn ensure_channel_owner(
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
pub(super) async fn guard_super_admin(
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

pub(super) async fn audit(
    state: &AppState,
    actor: &AuthedKey,
    action: &str,
    target: &str,
    detail: Value,
) {
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
    /// 所属渠道池。缺省（不传）= 进内置 default 池；显式传空数组 = 孤儿（对谁都不可达，
    /// 只在"先建好、稍后再放进专属池"时有意义）。元素可为池码字符串或带覆盖的对象。
    #[serde(default)]
    pub pools: Option<Vec<PoolMemberReq>>,
    /// 渠道高级设置（channels.settings 对象整体；已注册键见 docs/database.md：
    /// thinking_to_content / bill_by_response_model / strip_request_fields / pass_paths）。
    #[serde(default)]
    pub settings: Option<Value>,
    /// 相对成本系数（千分比；缺省 1000 = 按官方标价采购）。0 = 自建 / 免费上游。
    #[serde(default)]
    pub cost_milli: Option<i64>,
    /// 上游数据留存声明：none / transient / trains（缺省 = 未声明）。
    #[serde(default)]
    pub data_retention: Option<String>,
}

/// 数据留存声明的取值域。
///
/// 三档：`none` 上游不留存（可满足零留存要求）/ `transient` 短期留存但不用于训练 /
/// `trains` 可能用于训练。**未声明 ≠ none**——请求要求零留存时，未声明的渠道会被排除，
/// 因为"不知道对方留不留"不能当成"不留"。
fn ensure_data_retention(value: Option<&str>) -> Result<(), AppError> {
    match value {
        Some(v) if !matches!(v, "" | "none" | "transient" | "trains") => {
            Err(AppError::bad_request().with_param("data_retention"))
        }
        _ => Ok(()),
    }
}

/// 相对成本系数取值域：0（免费）～ 100×官方价，负数与离谱值都是手滑。
fn ensure_cost_milli(value: Option<i64>) -> Result<(), AppError> {
    match value {
        Some(v) if !(0..=100_000).contains(&v) => {
            Err(AppError::bad_request().with_param("cost_milli"))
        }
        _ => Ok(()),
    }
}

fn default_provider() -> String {
    "openai".to_owned()
}

/// 池成员写入形态：`"vip"` 或 `{"pool_code":"vip","priority_override":5,"weight_override":2}`。
/// 字符串形态保留是为了老客户端与测试脚本不用改。
#[derive(Deserialize)]
#[serde(untagged)]
pub enum PoolMemberReq {
    Code(String),
    Full(okapi_store::admin::PoolMember),
}

impl PoolMemberReq {
    fn into_member(self) -> okapi_store::admin::PoolMember {
        match self {
            Self::Code(pool_code) => okapi_store::admin::PoolMember {
                pool_code,
                priority_override: None,
                weight_override: None,
            },
            Self::Full(m) => m,
        }
    }
}

/// 池成员列表归一化：去空白、去重、校验池存在（不存在回 404 而非让 FK 报 500）。
async fn normalize_members(
    state: &AppState,
    reqs: Vec<PoolMemberReq>,
) -> Result<Vec<okapi_store::admin::PoolMember>, AppError> {
    let mut members: Vec<okapi_store::admin::PoolMember> = Vec::new();
    for r in reqs {
        let m = r.into_member();
        let pool_code = m.pool_code.trim();
        if pool_code.is_empty() || members.iter().any(|x| x.pool_code == pool_code) {
            continue;
        }
        members.push(okapi_store::admin::PoolMember {
            pool_code: pool_code.to_owned(),
            ..m
        });
    }
    let codes: Vec<String> = members.iter().map(|m| m.pool_code.clone()).collect();
    let known = sqlx::query_scalar!(
        r#"SELECT pool_code FROM channel_pools WHERE pool_code = ANY($1)"#,
        &codes
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if let Some(missing) = codes.iter().find(|c| !known.contains(c)) {
        return Err(
            AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param(missing.clone())
        );
    }
    Ok(members)
}

pub async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateChannelReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, _) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    super::ssrf::validate_api_base(&state, &req.api_base).await?;
    ensure_cost_milli(req.cost_milli)?;
    ensure_data_retention(req.data_retention.as_deref())?;
    let models: Vec<&str> = req.models.iter().map(String::as_str).collect();
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel(
        &state.pg,
        &req.name,
        &req.provider,
        &req.api_base,
        &req.credential,
        &models,
        req.trust_upstream_usage,
        state.master_key.as_deref(),
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
    if req.cost_milli.is_some() || req.data_retention.is_some() {
        okapi_store::admin::patch_channel(
            &state.pg,
            channel_id,
            okapi_store::admin::ChannelPatch {
                cost_milli: req.cost_milli,
                data_retention: req.data_retention.as_deref(),
                ..Default::default()
            },
        )
        .await?;
    }
    // 属主传播（#6267）：创建人即属主，own 范围据此过滤
    okapi_store::admin::set_channel_owner(&state.pg, channel_id, actor.user_id).await?;
    // provision 已把新渠道放进 default 池；显式给了 pools 才覆盖（空数组 = 孤儿）
    let pool_codes: Vec<String> = if let Some(reqs) = req.pools {
        let members = normalize_members(&state, reqs).await?;
        okapi_store::admin::set_channel_pools(&state.pg, channel_id, &members).await?;
        members.into_iter().map(|m| m.pool_code).collect()
    } else {
        vec![okapi_store::channels::DEFAULT_POOL.to_owned()]
    };
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.create",
        &channel_id.to_string(),
        json!({ "name": req.name, "provider": req.provider, "models": req.models, "pools": pool_codes }),
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
    // 最近测活结果一次 MGET 回填（Redis 30 天 TTL；没测过 / 已过期 = null）
    let ids: Vec<i64> = channels.iter().map(|c| c.id).collect();
    let mut last_tests = state.sched.channel_test_get_many(&ids).await;
    let data: Vec<Value> = channels
        .into_iter()
        .map(|c| {
            let keys: Vec<&okapi_store::admin::ChannelKeyRow> =
                keys.iter().filter(|k| k.channel_id == c.id).collect();
            json!({
                "id": c.id, "name": c.name, "provider": c.provider,
                "api_base": c.api_base, "status": c.status, "priority": c.priority,
                "models": c.models, "trust_upstream_usage": c.trust_upstream_usage,
                // 列表也带池：空数组 = 孤儿渠道（对谁都不可达），列表页要能一眼看见
                "pools": c.pools,
                "pool_members": c.pool_members,
                "cost_milli": c.cost_milli,
                "data_retention": c.data_retention,
                "keys": keys,
                "last_test": last_tests.remove(&c.id),
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
pub struct SetPoolsReq {
    pub pools: Vec<PoolMemberReq>,
}

/// 覆盖式设置渠道所属的池及成员级覆盖（可见性由池表达，docs/database.md §3.7）。
/// 空数组 = 从所有池移出 → 孤儿渠道，响应带 `orphan: true` 让前端明示后果。
pub async fn set_channel_pools(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<SetPoolsReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    let members = normalize_members(&state, req.pools).await?;
    okapi_store::admin::set_channel_pools(&state.pg, id, &members).await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.set_pools",
        &id.to_string(),
        json!({ "pools": members }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "orphan": members.is_empty() })))
}

/// 已支持的上游协议（docs/database.md channels.provider）。
const PROVIDERS: [&str; 5] = [
    "openai",
    "openai_compat",
    "anthropic",
    "gemini",
    "custom_pass",
];

#[derive(Deserialize)]
pub struct PatchChannelReq {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    /// 请求模型 → 上游模型映射（对象）。
    #[serde(default)]
    pub model_mapping: Option<Value>,
    /// channels.settings 整体覆盖（对象）。
    #[serde(default)]
    pub settings: Option<Value>,
    /// 能力声明（tools/vision），驱动能力感知路由。
    #[serde(default)]
    pub capabilities: Option<Value>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub trust_upstream_usage: Option<bool>,
    /// 相对成本系数（千分比）。
    #[serde(default)]
    pub cost_milli: Option<i64>,
    /// 数据留存声明；空串 = 清除声明。
    #[serde(default)]
    pub data_retention: Option<String>,
}

/// PATCH /admin/channels/{id}：改渠道配置（缺省字段不动）。
/// 启停仍走 `/status`，凭证走 `/credential`——三者审计语义不同，不合并。
pub async fn update_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<PatchChannelReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;

    if let Some(provider) = &req.provider
        && !PROVIDERS.contains(&provider.as_str())
    {
        return Err(AppError::bad_request().with_param("provider"));
    }
    if let Some(api_base) = &req.api_base {
        super::ssrf::validate_api_base(&state, api_base).await?;
    }
    for (field, value) in [
        ("model_mapping", req.model_mapping.as_ref()),
        ("settings", req.settings.as_ref()),
        ("capabilities", req.capabilities.as_ref()),
    ] {
        if let Some(v) = value
            && !v.is_object()
        {
            return Err(AppError::bad_request().with_param(field));
        }
    }
    let models = match &req.models {
        Some(list) if list.iter().any(|m| m.trim().is_empty()) => {
            return Err(AppError::bad_request().with_param("models"));
        }
        Some(list) => Some(json!(list)),
        None => None,
    };
    ensure_cost_milli(req.cost_milli)?;
    ensure_data_retention(req.data_retention.as_deref())?;

    let patch = okapi_store::admin::ChannelPatch {
        name: req.name.as_deref(),
        provider: req.provider.as_deref(),
        api_base: req.api_base.as_deref(),
        models: models.as_ref(),
        model_mapping: req.model_mapping.as_ref(),
        settings: req.settings.as_ref(),
        capabilities: req.capabilities.as_ref(),
        priority: req.priority,
        trust_upstream_usage: req.trust_upstream_usage,
        cost_milli: req.cost_milli,
        data_retention: req.data_retention.as_deref(),
    };
    if !okapi_store::admin::patch_channel(&state.pg, id, patch).await? {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.update",
        &id.to_string(),
        json!({
            "name": req.name, "provider": req.provider, "api_base": req.api_base,
            "models": req.models, "priority": req.priority,
            "trust_upstream_usage": req.trust_upstream_usage,
            "cost_milli": req.cost_milli,
            "data_retention": req.data_retention,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// DELETE /admin/channels/{id}：软删除（历史账单按 channel_id 仍可回溯）。
pub async fn delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    if !okapi_store::admin::soft_delete_channel(&state.pg, id).await? {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.invalidate_routing_caches();
    audit(&state, &actor, "channel.delete", &id.to_string(), json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct RotateCredentialReq {
    pub credential: String,
    /// 多把 key 的渠道必填；单把可省。
    #[serde(default)]
    pub channel_key_id: Option<i64>,
}

/// POST /admin/channels/{id}/credential：轮换上游凭证并复位 key 状态机。
/// 明文凭证不进审计 detail。
pub async fn rotate_channel_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<RotateCredentialReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    if req.credential.trim().is_empty() {
        return Err(AppError::bad_request().with_param("credential"));
    }
    let outcome = okapi_store::admin::rotate_channel_credential(
        &state.pg,
        id,
        req.channel_key_id,
        req.credential.trim(),
        state.master_key.as_deref(),
    )
    .await?;
    let channel_key_id = match outcome {
        okapi_store::admin::RotateOutcome::Rotated(key_id) => key_id,
        okapi_store::admin::RotateOutcome::NotFound => {
            return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
        }
        okapi_store::admin::RotateOutcome::Ambiguous => {
            return Err(AppError::bad_request().with_param("channel_key_id"));
        }
    };
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.rotate_credential",
        &id.to_string(),
        json!({ "channel_key_id": channel_key_id }),
    )
    .await;
    Ok(Json(
        json!({ "ok": true, "channel_key_id": channel_key_id }),
    ))
}

#[derive(Deserialize)]
pub struct PatchChannelKeyReq {
    #[serde(default)]
    pub weight: Option<i32>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub max_concurrency: Option<Option<i32>>,
    /// 状态：1 启用 / 2 停用。置状态同时清零失败计数、冷却与 last_error。
    #[serde(default)]
    pub status: Option<i16>,
}

/// PATCH /admin/channels/{id}/keys/{key_id}：调单把 key 的权重、并发上限与状态。
///
/// `status=1` 即「重新启用」——被 401/403 打成 `status=6`（凭证失效，无冷却不自愈）的 key
/// 此前只能靠重置凭证救回，运维排查完上游侧问题后没有一键恢复的路径。
pub async fn update_channel_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, key_id)): Path<(i64, i64)>,
    Json(req): Json<PatchChannelKeyReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    if req.weight.is_some_and(|w| w < 0) {
        return Err(AppError::bad_request().with_param("weight"));
    }
    if req.max_concurrency.flatten().is_some_and(|c| c <= 0) {
        return Err(AppError::bad_request().with_param("max_concurrency"));
    }
    // 只放 1/2：其余状态位是状态机自己的（冷却/限流/配额/失效），手工写进去只会骗过调度器
    if req.status.is_some_and(|s| !matches!(s, 1 | 2)) {
        return Err(AppError::bad_request().with_param("status"));
    }
    let hit = okapi_store::admin::patch_channel_key(
        &state.pg,
        id,
        key_id,
        req.weight,
        req.max_concurrency,
        req.status,
    )
    .await?;
    if !hit {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.update_key",
        &format!("{id}/{key_id}"),
        json!({
            "weight": req.weight,
            "max_concurrency": req.max_concurrency,
            "status": req.status,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- API key 管控（管理面：限额与分组覆盖）----

#[derive(Deserialize)]
pub struct AdminPatchKeyReq {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub expires_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub model_allowlist: Option<Option<Vec<String>>>,
    /// key 级定价组覆盖；null = 回落用户组。
    #[serde(default, deserialize_with = "super::double_option")]
    pub group_override: Option<Option<String>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub rpm_limit: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub tpm_limit: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub rpd_limit: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub daily_token_limit: Option<Option<i64>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub max_concurrency: Option<Option<i32>>,
    /// IP 白名单（地址 / CIDR 数组；null / 空 = 解除）。
    #[serde(default, deserialize_with = "super::double_option")]
    pub ip_allowlist: Option<Option<Vec<String>>>,
}

/// 限额一律取正数：0 或负数会把 key 限死成不可用，属于误配而非合法配置。
// 入参来自 double_option 的三态补丁值，豁免理由同 console::double_option
#[allow(clippy::option_option)]
fn ensure_positive(field: &str, value: Option<Option<i64>>) -> Result<(), AppError> {
    match value.flatten() {
        Some(v) if v <= 0 => Err(AppError::bad_request().with_param(field.to_owned())),
        _ => Ok(()),
    }
}

/// PATCH /admin/keys/{id}：管理面改任意用户 key 的全部可写字段（含限额与分组覆盖）。
pub async fn patch_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<AdminPatchKeyReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_MANAGE).await?;
    if let Some(status) = req.status
        && !matches!(status, 1 | 2)
    {
        return Err(AppError::bad_request().with_param("status"));
    }
    ensure_positive("rpm_limit", req.rpm_limit.map(|v| v.map(i64::from)))?;
    ensure_positive("tpm_limit", req.tpm_limit.map(|v| v.map(i64::from)))?;
    ensure_positive("rpd_limit", req.rpd_limit.map(|v| v.map(i64::from)))?;
    ensure_positive("daily_token_limit", req.daily_token_limit)?;
    ensure_positive(
        "max_concurrency",
        req.max_concurrency.map(|v| v.map(i64::from)),
    )?;
    // 分组前置校验：直接撞 FK 只能回 500，拿不到可渲染的 error_code
    if let Some(Some(group)) = &req.group_override
        && !okapi_store::admin::price_group_exists(&state.pg, group).await?
    {
        return Err(AppError::bad_request().with_param("group_override"));
    }

    let patch = okapi_store::admin::ApiKeyPatch {
        name: req.name.clone().map(|n| n.trim().to_owned()),
        status: req.status,
        expires_at: req.expires_at,
        model_allowlist: req
            .model_allowlist
            .clone()
            .map(super::portal::normalize_allowlist),
        group_override: req.group_override.clone(),
        rpm_limit: req.rpm_limit,
        tpm_limit: req.tpm_limit,
        rpd_limit: req.rpd_limit,
        daily_token_limit: req.daily_token_limit,
        max_concurrency: req.max_concurrency,
        ip_allowlist: match req.ip_allowlist.clone() {
            Some(list) => Some(super::portal::normalize_ip_allowlist(list)?),
            None => None,
        },
    };
    let touched = okapi_store::admin::patch_api_key(&state.pg, id, None, &patch).await?;
    let Some(touched) = touched else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    state.sched.auth_del(&touched.key_hash).await;
    audit(
        &state,
        &actor,
        "apikey.update",
        &id.to_string(),
        json!({
            "user_id": touched.user_id, "status": req.status,
            "group_override": req.group_override, "rpm_limit": req.rpm_limit,
            "tpm_limit": req.tpm_limit, "rpd_limit": req.rpd_limit,
            "daily_token_limit": req.daily_token_limit,
            "max_concurrency": req.max_concurrency, "expires_at": req.expires_at,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "user_id": touched.user_id })))
}

/// DELETE /admin/keys/{id}：管理面吊销任意 key。
pub async fn delete_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_MANAGE).await?;
    let touched = okapi_store::admin::soft_delete_api_key(&state.pg, id, None).await?;
    let Some(touched) = touched else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    state.sched.auth_del(&touched.key_hash).await;
    audit(
        &state,
        &actor,
        "apikey.delete",
        &id.to_string(),
        json!({ "user_id": touched.user_id }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "user_id": touched.user_id })))
}

// ---- 模型与定价 ----

#[derive(Deserialize)]
pub struct UpsertGroupReq {
    pub group_code: String,
    /// 分组倍率（十进制字符串）。
    pub group_ratio: String,
    #[serde(default)]
    pub description: String,
    /// 该分组的用户打哪个渠道池；null/缺省 = 内置 default 池（分组必有池）。
    #[serde(default)]
    pub pool_code: Option<String>,
    /// 用户可否在门户为自己的 key 自选此分组（new-api UserUsableGroups 的对应物）。
    #[serde(default)]
    pub self_select: bool,
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
    let pool_code = req
        .pool_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(okapi_store::channels::DEFAULT_POOL);
    ensure_pool_exists(&state, pool_code).await?;
    okapi_store::admin::upsert_price_group(
        &state.pg,
        okapi_store::admin::PriceGroupInput {
            group_code: &req.group_code,
            group_ratio: &req.group_ratio,
            description: &req.description,
            pool_code: Some(pool_code),
            self_select: req.self_select,
        },
    )
    .await?;
    // 分组的池变了，绑定该组的 key 鉴权缓存里还是旧池——与改角色同一动作，全量失效
    state.sched.auth_flush().await;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "pricing.upsert_group",
        &req.group_code,
        json!({ "group_ratio": req.group_ratio, "pool_code": pool_code, "self_select": req.self_select }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// 池存在性校验：不存在回 404 带 param，而不是让外键违约变成 500。
async fn ensure_pool_exists(state: &AppState, pool_code: &str) -> Result<(), AppError> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM channel_pools WHERE pool_code = $1) AS "e!""#,
        pool_code
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if exists {
        Ok(())
    } else {
        Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("pool_code"))
    }
}

/// 渠道池取值：与库 CHECK 一致，前端下拉也用这份清单。
const ROUTING_STRATEGIES: &[&str] = &["priority_weighted", "least_latency"];

#[derive(Deserialize)]
pub struct UpsertPoolReq {
    pub pool_code: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub routing_strategy: Option<String>,
    /// 本池对某模型无可用候选时退到的池（单跳；不能是自己）。
    #[serde(default)]
    pub fallback_pool_code: Option<String>,
}

/// 渠道池 upsert（池 = 一组渠道 + 在这组里怎么选，docs/database.md §3.7）。
pub async fn upsert_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertPoolReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let strategy = req
        .routing_strategy
        .as_deref()
        .unwrap_or("priority_weighted");
    if !ROUTING_STRATEGIES.contains(&strategy) {
        return Err(AppError::bad_request().with_param("routing_strategy"));
    }
    let pool_code = req.pool_code.trim();
    if pool_code.is_empty() {
        return Err(AppError::bad_request().with_param("pool_code"));
    }
    let fallback = req
        .fallback_pool_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(fb) = fallback {
        if fb == pool_code {
            return Err(AppError::bad_request().with_param("fallback_pool_code"));
        }
        ensure_pool_exists(&state, fb).await?;
    }
    okapi_store::admin::upsert_channel_pool(
        &state.pg,
        pool_code,
        &req.description,
        strategy,
        fallback,
    )
    .await?;
    // 池的策略 / 降级目标随鉴权缓存下发，改了要让持有旧值的 key 重新解析
    state.sched.auth_flush().await;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.upsert_pool",
        pool_code,
        json!({ "routing_strategy": strategy, "fallback_pool_code": fallback }),
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

/// 全局设置写入（如 site_notice / model_rpm_limits）。
pub async fn set_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetSettingReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    okapi_store::admin::set_setting(&state.pg, &req.key, &req.value, actor.user_id).await?;
    state.invalidate_routing_caches();
    // settings 热路径缓存按键失效：同进程立即生效（公告发布、限流阈值调整不必等 60s TTL），
    // 多副本靠 TTL 收敛——与鉴权缓存"本机即时、跨副本 TTL"同一取舍
    state.settings_cache.invalidate(&req.key).await;
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
    /// 音频输入倍率（相对文本；gpt-4o-audio 官方 16）。
    #[serde(default = "default_one")]
    pub audio_ratio: String,
    /// 音频输出倍率（叠乘在 audio_ratio 之上；官方 2）。缺省 1 且 audio_ratio 也为 1 时，
    /// 音频输出回落到 completion_ratio（详见 DESIGN §3.2）。
    #[serde(default = "default_one")]
    pub audio_completion_ratio: String,
    /// 图片输入倍率（相对文本）。
    #[serde(default = "default_one")]
    pub image_ratio: String,
    /// service_tier 档位倍率（如 {"flex":"0.5","priority":"2.0"}；
    /// None=不改动，空对象=清除，DESIGN §3-4.5）。
    #[serde(default)]
    pub tier_ratios: Option<serde_json::Map<String, Value>>,
    /// 阶梯计价表 `"0:2.5,128000:5"`（from_tokens:USD_per_1M，首档须从 0 起、严格升序）。
    /// 给非空串 = 切 tiered 模式（`model_ratio` 由阶梯查表替代，其余倍率轴照旧）；
    /// 给空串 = 切回 ratio；None = 不改动模式。
    #[serde(default)]
    pub tier_expr: Option<String>,
    /// 模型级降级链（DESIGN §3.4.1）：零可用候选时按序改投。
    /// None=不改动，空数组=清除。条目须为已存在的模型名——写入时校验，
    /// 拼错的降级模型只会在最脆弱的时刻（主模型已全挂）暴露，必须前置拦截。
    #[serde(default)]
    pub fallback_models: Option<Vec<String>>,
}

/// 降级链上限：链是兜底不是路由表，过长说明在拿降级当调度用。
const MAX_FALLBACK_CHAIN: usize = 8;

/// 校验并落库模型降级链：归一化（去空白/自引用/保序去重）→ 上限 →
/// 条目须为已存在模型（不要求 active：临时停用的模型保留在链上，恢复即生效）。
async fn apply_fallback_models(
    state: &AppState,
    model_id: i64,
    model_name: &str,
    raw_chain: &[String],
) -> Result<(), AppError> {
    let mut chain: Vec<String> = Vec::new();
    for entry in raw_chain {
        let name = entry.trim();
        if name.is_empty() || name == model_name || chain.iter().any(|c| c == name) {
            continue;
        }
        chain.push(name.to_owned());
    }
    if chain.len() > MAX_FALLBACK_CHAIN {
        return Err(AppError::bad_request().with_param("fallback_models"));
    }
    let known = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM models WHERE model_name = ANY($1)"#,
        &chain
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if usize::try_from(known).unwrap_or(0) != chain.len() {
        return Err(AppError::bad_request().with_param("fallback_models"));
    }
    sqlx::query!(
        r#"UPDATE models SET fallback_models = $2, updated_at = now() WHERE id = $1"#,
        model_id,
        serde_json::json!(chain)
    )
    .execute(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(())
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
        &req.audio_ratio,
        &req.audio_completion_ratio,
        &req.image_ratio,
    ] {
        if literal.parse::<okapi_pricing::RatioFp>().is_err() {
            return Err(AppError::bad_request().with_param("ratio"));
        }
    }
    let axes = okapi_store::admin::RatioAxes {
        model: &req.model_ratio,
        completion: &req.completion_ratio,
        cache: &req.cache_ratio,
        cache_write: &req.cache_write_ratio,
        audio: &req.audio_ratio,
        audio_completion: &req.audio_completion_ratio,
        image: &req.image_ratio,
    };
    // 阶梯表非空 → tiered；空串 → 切回 ratio；None → 保持既有模式的 ratio 写入路径。
    // 校验放在写库前：阶梯表配错只会在**编译价簿**时炸，那时改动已发布，整本价簿一起装载失败。
    let tier_expr = req.tier_expr.as_deref().map(str::trim).filter(|e| !e.is_empty());
    let model_id = match tier_expr {
        Some(expr) => {
            okapi_pricing::TierTable::check_expr(expr)
                .map_err(|reason| AppError::bad_request().with_param(format!("tier_expr:{reason}")))?;
            okapi_store::admin::upsert_model_tiered(&state.pg, &req.model_name, axes, expr).await?
        }
        None => okapi_store::admin::upsert_model_ratio(&state.pg, &req.model_name, axes).await?,
    };
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
    if let Some(raw_chain) = &req.fallback_models {
        apply_fallback_models(&state, model_id, &req.model_name, raw_chain).await?;
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
            "audio_ratio": req.audio_ratio,
            "audio_completion_ratio": req.audio_completion_ratio,
            "image_ratio": req.image_ratio,
            "tier_expr": req.tier_expr,
            "fallback_models": req.fallback_models,
        }),
    )
    .await;
    Ok(Json(json!({ "model_id": model_id })))
}

// ---- 定价规则栈（DESIGN §3.4）----

#[derive(Deserialize)]
pub struct UpsertRuleReq {
    pub rule_code: String,
    /// volume | time_based | discount | surge。
    pub rule_type: String,
    /// 命中时施加的乘数（十进制字符串，禁浮点入库）。
    pub multiplier: String,
    /// volume：本月累计 token 阈值（与消费额阈值二者至少一项；同配 = AND）。
    #[serde(default)]
    pub min_monthly_tokens: Option<u64>,
    /// volume：本月累计消费阈值 micro-USD（贵模型大客户用量少但付费多，§11.5）。
    #[serde(default)]
    pub min_monthly_spend_micro: Option<u64>,
    /// time_based 必填：[start, end) 本地分钟窗，允许跨零点回绕。
    #[serde(default)]
    pub start_minute: Option<u16>,
    #[serde(default)]
    pub end_minute: Option<u16>,
    /// time_based 可选：星期列表 0=周日…6=周六（缺省每天；与分钟窗同为 UTC 钟源）。
    #[serde(default)]
    pub weekdays: Option<Vec<u8>>,
    /// 多命中叠加语义：stackable（缺省，连乘）/ exclusive（桶内 priority 最高独占）/
    /// best_for_user（桶内取对用户最优一条）。
    #[serde(default)]
    pub stacking_mode: Option<String>,
    /// 作用域选择器 {"groups":[],"models":[],"users":[]}；缺省不限。
    #[serde(default)]
    pub scope: Option<Value>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub valid_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub valid_to: Option<chrono::DateTime<chrono::Utc>>,
}

const fn default_true() -> bool {
    true
}

/// 按 rule_type 组装并校验 params——非法组合一律 400，绝不让"配得上却永不命中"
/// 的规则进库（本端点补齐前 pricing_rules 只能手写 SQL，正是该类事故的来源）。
fn rule_params(req: &UpsertRuleReq) -> Result<Value, AppError> {
    if req.multiplier.parse::<okapi_pricing::RatioFp>().is_err() {
        return Err(AppError::bad_request().with_param("multiplier"));
    }
    let mut params = serde_json::Map::new();
    params.insert(
        "multiplier".to_owned(),
        Value::String(req.multiplier.clone()),
    );
    match req.rule_type.as_str() {
        "volume" => {
            // 双阈值轴至少一项 > 0；两轴全空的 volume 就是无条件规则冒充，拒绝
            let tokens = req.min_monthly_tokens.unwrap_or(0);
            let spend = req.min_monthly_spend_micro.unwrap_or(0);
            if tokens == 0 && spend == 0 {
                return Err(AppError::bad_request().with_param("volume_threshold"));
            }
            if tokens > 0 {
                params.insert("min_monthly_tokens".to_owned(), json!(tokens));
            }
            if spend > 0 {
                params.insert("min_monthly_spend_micro".to_owned(), json!(spend));
            }
        }
        "time_based" => {
            let (start, end) = req
                .start_minute
                .zip(req.end_minute)
                .ok_or_else(|| AppError::bad_request().with_param("minute_window"))?;
            if start >= 1440 || end >= 1440 {
                return Err(AppError::bad_request().with_param("minute_window"));
            }
            // start == end 是空窗（永不命中），对齐 new-api rc.27 #6934 的反面语义：
            // 与其静默退化成全天生效，不如在配置期就拒绝
            if start == end {
                return Err(AppError::bad_request().with_param("minute_window_empty"));
            }
            params.insert("start_minute".to_owned(), json!(start));
            params.insert("end_minute".to_owned(), json!(end));
            if let Some(days) = &req.weekdays {
                // 空列表/非法值与"缺省每天"是三回事：空掩码永不命中，属配置错误
                let mask = okapi_pricing::WeekdayMask::from_days(days)
                    .ok_or_else(|| AppError::bad_request().with_param("weekdays"))?;
                params.insert("weekdays".to_owned(), json!(mask.days()));
            }
        }
        "discount" | "surge" => {}
        _ => return Err(AppError::bad_request().with_param("rule_type")),
    }
    if let Some(raw) = req.stacking_mode.as_deref() {
        let mode = okapi_pricing::Stacking::parse(raw)
            .ok_or_else(|| AppError::bad_request().with_param("stacking_mode"))?;
        // 缺省值不落库：params 里只存显式偏离，与 weekdays 同一策略
        if mode != okapi_pricing::Stacking::Stackable {
            params.insert("stacking_mode".to_owned(), json!(mode.tag()));
        }
    }
    Ok(Value::Object(params))
}

/// 定价规则 upsert（改动后需 publish 才进价簿；与模型/分组同一发布闸）。
pub async fn upsert_pricing_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertRuleReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let params = rule_params(&req)?;
    let scope = req.scope.clone().unwrap_or_else(|| json!({}));
    if !scope.is_object() {
        return Err(AppError::bad_request().with_param("scope"));
    }
    if req
        .valid_from
        .zip(req.valid_to)
        .is_some_and(|(f, t)| f >= t)
    {
        return Err(AppError::bad_request().with_param("valid_window"));
    }
    okapi_store::admin::upsert_pricing_rule(
        &state.pg,
        okapi_store::admin::PricingRuleInput {
            rule_code: &req.rule_code,
            rule_type: &req.rule_type,
            scope: &scope,
            params: &params,
            priority: req.priority,
            enabled: req.enabled,
            valid_from: req.valid_from,
            valid_to: req.valid_to,
        },
    )
    .await?;
    audit(
        &state,
        &actor,
        "pricing.upsert_rule",
        &req.rule_code,
        json!({ "rule_type": req.rule_type, "params": params, "enabled": req.enabled }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// 规则列表（按生效叠加序返回，管理端据此核对多规则连乘结果）。
pub async fn list_pricing_rules(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let rows = okapi_store::admin::list_pricing_rules(&state.pg).await?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "rule_code": r.rule_code,
                "rule_type": r.rule_type,
                "scope": r.scope,
                "params": r.params,
                "priority": r.priority,
                "enabled": r.enabled,
                "valid_from": r.valid_from,
                "valid_to": r.valid_to,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

pub async fn delete_pricing_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(rule_code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if !okapi_store::admin::delete_pricing_rule(&state.pg, &rule_code).await? {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    audit(&state, &actor, "pricing.delete_rule", &rule_code, json!({})).await;
    Ok(Json(json!({ "ok": true })))
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

#[derive(Deserialize)]
pub struct SetMultiplierReq {
    /// 十进制字符串（如 "1.25"、"0.8"）。0 = 免单；禁浮点入库故不收 f64。
    pub multiplier: String,
}

/// POST /admin/users/{id}/multiplier：设用户个人计价系数。
///
/// 权限走 `pricing.write` 而非 `user.manage`：它是计价链上与模型倍率、分组倍率并列的
/// 一个乘数（`amount = eff × model × group × **user** × 规则`），改它等于改这个人的价目表。
///
/// 取值 [0, 1000]：0 = 免单（内部账号常用），上限只是防手滑把小数点点丢。
/// 改完必须刷鉴权缓存——系数随 `AuthedKey` 缓存，不刷的话最长一分钟仍按旧价计费。
pub async fn set_user_multiplier(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(req): Json<SetMultiplierReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let raw = req.multiplier.trim();
    let value: f64 = raw
        .parse()
        .map_err(|_| AppError::bad_request().with_param("multiplier"))?;
    if !(0.0..=1000.0).contains(&value) || !value.is_finite() {
        return Err(AppError::bad_request().with_param("multiplier"));
    }
    if !okapi_store::admin::set_user_multiplier(&state.pg, user_id, raw).await? {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "user.set_multiplier",
        &user_id.to_string(),
        json!({ "multiplier": raw }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "multiplier": raw })))
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
    // upsert 可能改了已绑定管理员的权限集合：全量失效鉴权缓存（与改用户角色同一动作）
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "role.upsert",
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

/// 按 request_id 查账单摘要（billing.read）：退款前的预览步——
/// 让管理员先看清"退的是谁的哪笔、多少钱、当前什么状态"，而不是对着一个
/// UUID 盲按退款按钮。LEFT JOIN 用户名，退款资格由 status 判定。
pub async fn billing_record_lookup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::BILLING_READ).await?;
    let row = sqlx::query!(
        r#"
        SELECT br.user_id, u.username, br.model_name, br.status, br.log_type,
               br.amount_micro, br.original_amount_micro,
               br.prompt_tokens, br.completion_tokens,
               br.error_code, br.created_at
        FROM billing_records br
        LEFT JOIN users u ON u.id = br.user_id
        WHERE br.request_id = $1
        ORDER BY br.created_at DESC
        LIMIT 1
        "#,
        request_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let Some(r) = row else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("request_id"));
    };
    Ok(Json(json!({
        "request_id": request_id,
        "user_id": r.user_id,
        "username": r.username,
        "model": r.model_name,
        "status": r.status,
        "log_type": r.log_type,
        "amount_micro": r.amount_micro,
        "original_amount_micro": r.original_amount_micro,
        "prompt_tokens": r.prompt_tokens,
        "completion_tokens": r.completion_tokens,
        "error_code": r.error_code,
        "created_at": r.created_at,
        // 只有成功扣费（committed）的记录有款可退
        "refundable": r.status == 20,
    })))
}

/// 按日志退款（§5.3）：事件溯源冲销，账单/统计/余额三处口径自动一致；幂等。
/// 三种结局分开说（此前"已退过"与"id 不存在"都混在 404 里，管理员分不清
/// "打错了"还是"重复点了但幂等安全"）：
/// - 成功 → outcome=refunded + 金额与退款后余额；
/// - 已退过 → 200 outcome=already_refunded（幂等语义，不是错误）；
/// - 存在但未成功扣费（失败/预扣中）→ 409 refund_not_committed（无款可退）；
/// - 不存在 → 404。
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
        let status = sqlx::query_scalar!(
            r#"SELECT status FROM billing_records WHERE request_id = $1
               ORDER BY created_at DESC LIMIT 1"#,
            req.request_id
        )
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        return match status {
            Some(30) => Ok(Json(json!({ "outcome": "already_refunded" }))),
            Some(_) => Err(AppError::new(StatusCode::CONFLICT, "refund_not_committed")),
            None => {
                Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("request_id"))
            }
        };
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
        "outcome": "refunded",
        "user_id": refund.user_id,
        "refunded_micro": refund.amount.as_micros(),
        "balance_after_micro": balance_after.as_micros(),
    })))
}

// ---- 代客查看（#1790-2，强审计）----

#[derive(Deserialize)]
pub struct UserListQuery {
    /// 用户名/邮箱模糊匹配（走 bind 参数，不拼字符串）。
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// GET /admin/users：用户列表（此前只能按 ID 操作，没有列表入口）。
/// 只读点 `user.read` 即可（与 /admin/keys 列表一致，§11.6 读写分离）：此前守的是
/// `user.manage`，只读运营角色在侧栏看得见"用户"却点进去 403——文档、导航与后端三处
/// 里后端是那个错的。管理动作（manage/credit/role/groups）仍要 `user.manage`。
pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UserListQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::USER_READ).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    // 空查询用 NULL 表示"不过滤"，避免 '%%' 走不上索引的语义歧义
    let needle =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{s}%"));

    let rows = sqlx::query!(
        r#"
        SELECT id, username, email, role, status, balance_micro, admin_role_id,
               price_multiplier::text AS "multiplier!", created_at
        FROM users
        WHERE deleted_at IS NULL
          AND ($1::text IS NULL OR username ILIKE $1 OR email ILIKE $1)
        ORDER BY id DESC
        LIMIT $2 OFFSET $3
        "#,
        needle,
        limit,
        offset
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let total = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*)::bigint AS "c!" FROM users
        WHERE deleted_at IS NULL
          AND ($1::text IS NULL OR username ILIKE $1 OR email ILIKE $1)
        "#,
        needle
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "username": r.username,
                "email": r.email,
                "role": r.role,
                "status": r.status,
                "balance_micro": r.balance_micro,
                "admin_role_id": r.admin_role_id,
                "price_multiplier": r.multiplier,
                "created_at": r.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "total": total, "data": data })))
}

/// GET /admin/roles：自定义管理角色列表（供分配下拉使用）。
pub async fn list_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::ROLE_MANAGE).await?;
    let rows = sqlx::query!(
        r#"SELECT id, role_code, display_name, permissions FROM admin_roles ORDER BY id"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "role_code": r.role_code,
                "display_name": r.display_name,
                "permissions": r.permissions,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

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

#[derive(Deserialize)]
pub struct UserUsageQuery {
    #[serde(default)]
    pub days: Option<u32>,
}

/// GET /admin/users/{id}/usage：代客视角的用量与余额变动史（老 ok-api 用户详情
/// UsageOverviewTab / UsageTrendChart 的吸收）。
///
/// 管理员调余额、处理"为什么扣这么多"之前要先看两件事：他平时花多少（按日/按模型，
/// CH mv_key_model_day 用户前缀）与上次动过什么账（PG billing_events，含 actor——
/// 这是管理面，谁调的账要看得见，与门户端点隐去管理员 id 相反）。
/// CH 未启用时用量部分为空数组而非整体 501：余额变动史不依赖 CH，不该被连坐。
pub async fn user_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Query(q): Query<UserUsageQuery>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_ASSIST).await?;
    let days = q.days.unwrap_or(7).clamp(1, 90);

    let mut daily: Vec<Value> = Vec::new();
    let mut by_model: Vec<Value> = Vec::new();
    if let Some(ch) = state.ch.as_ref() {
        let rows = ch
            .query_json_each_row(&format!(
                "SELECT day, model, countMerge(requests) AS reqs, sumMerge(amount) AS spend, \
                        sumMerge(prompt_tokens) + sumMerge(completion_tokens) AS toks \
                 FROM mv_key_model_day WHERE user_id = {user_id} AND day >= today() - {days} \
                 GROUP BY day, model ORDER BY day"
            ))
            .await?;
        // BTreeMap：按日键有序输出，前端直接画不用再排
        let mut per_day: std::collections::BTreeMap<String, (i64, i64)> =
            std::collections::BTreeMap::new();
        let mut per_model: std::collections::HashMap<String, (i64, i64, i64)> =
            std::collections::HashMap::new();
        for r in &rows {
            let day = r.get("day").and_then(Value::as_str).unwrap_or_default();
            let model = r.get("model").and_then(Value::as_str).unwrap_or_default();
            let (reqs, spend, toks) = (
                super::stats::ch_i64(r, "reqs"),
                super::stats::ch_i64(r, "spend"),
                super::stats::ch_i64(r, "toks"),
            );
            let d = per_day.entry(day.to_owned()).or_default();
            d.0 += reqs;
            d.1 += spend;
            let m = per_model.entry(model.to_owned()).or_default();
            m.0 += reqs;
            m.1 += spend;
            m.2 += toks;
        }
        daily = per_day
            .into_iter()
            .map(|(day, (reqs, spend))| json!({ "day": day, "requests": reqs, "amount_micro": spend }))
            .collect();
        let mut models: Vec<(String, (i64, i64, i64))> = per_model.into_iter().collect();
        models.sort_by_key(|m| std::cmp::Reverse(m.1.1));
        by_model = models
            .into_iter()
            .take(10)
            .map(|(model, (reqs, spend, toks))| {
                json!({ "model": model, "requests": reqs, "amount_micro": spend, "tokens": toks })
            })
            .collect();
    }

    let events = sqlx::query!(
        r#"SELECT event_id, event_type, delta_micro, balance_after_micro, payload, actor, created_at
           FROM billing_events
           WHERE user_id = $1
             AND event_type IN ('recharge', 'adjust', 'refund', 'expire')
             AND delta_micro <> 0
           ORDER BY event_id DESC LIMIT 10"#,
        user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let ledger: Vec<Value> = events
        .into_iter()
        .map(|e| {
            json!({
                "event_id": e.event_id,
                "event_type": e.event_type,
                "delta_micro": e.delta_micro,
                "balance_after_micro": e.balance_after_micro,
                "actor": e.actor,
                "tags": e.payload.as_ref().and_then(|p| p.get("tags")).cloned().unwrap_or(Value::Array(vec![])),
                "reason": e.payload.as_ref().and_then(|p| p.get("reason")).cloned(),
                "created_at": e.created_at.to_rfc3339(),
            })
        })
        .collect();

    audit(
        &state,
        &actor,
        "user.assist.usage",
        &user_id.to_string(),
        json!({ "days": days }),
    )
    .await;

    Ok(Json(json!({
        "days": days,
        "stats_available": state.ch.is_some(),
        "daily": daily,
        "by_model": by_model,
        "ledger": ledger,
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
    /// new-api 音频输入倍率（相对文本）。
    #[serde(default)]
    pub audio_ratio: serde_json::Map<String, Value>,
    /// new-api 音频输出倍率（叠乘在 audio_ratio 之上）。
    #[serde(default)]
    pub audio_completion_ratio: serde_json::Map<String, Value>,
    /// new-api 图片输入倍率。
    #[serde(default)]
    pub image_ratio: serde_json::Map<String, Value>,
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
        let pick = |m: &serde_json::Map<String, Value>| {
            m.get(model)
                .and_then(ratio_literal)
                .unwrap_or_else(|| "1".to_owned())
        };
        let audio = pick(&req.audio_ratio);
        let audio_completion = pick(&req.audio_completion_ratio);
        let image = pick(&req.image_ratio);
        // 复用定价域解析器做入库前校验（纯整数定点，禁浮点）
        if [
            &model_ratio,
            &completion,
            &cache,
            &cache_write,
            &audio,
            &audio_completion,
            &image,
        ]
        .iter()
        .any(|s| s.parse::<okapi_pricing::RatioFp>().is_err())
        {
            skipped.push(model.clone());
            continue;
        }
        okapi_store::admin::upsert_model_ratio(
            &state.pg,
            model,
            okapi_store::admin::RatioAxes {
                model: &model_ratio,
                completion: &completion,
                cache: &cache,
                cache_write: &cache_write,
                audio: &audio,
                audio_completion: &audio_completion,
                image: &image,
            },
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
#[derive(Deserialize, Default)]
pub struct TestChannelReq {
    /// 给了就真发一次 1 token 的最小补全验这个模型；不给只验凭证与连通性。
    #[serde(default)]
    pub model: Option<String>,
}

pub async fn test_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
    // Option<Json>：老调用方不带 body（也就没有 content-type），必须仍然能测
    body: Option<Json<TestChannelReq>>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let model = req.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
    let result = probe_channel(&state, channel_id, model).await?;
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

/// 探测补全的输出上限。取 16 而非 1：实测有上游把 `max_tokens=1` 直接 400
/// （`max_tokens must be greater than 2`），好模型会被误报成不可用；16 仍然便宜到可忽略，
/// 兼容性却好得多（推理模型的最小输出预算也常常大于 1）。
const PROBE_MAX_TOKENS: u32 = 16;

/// 按协议与探测范围拼探测请求：`(method, url, body)`。
/// 无 model = GET 协议各自的模型列表端点；有 model = POST 一次最小补全。
fn probe_request(
    provider: &str,
    base: &str,
    model: Option<&str>,
) -> Result<(axum::http::Method, String, bytes::Bytes), AppError> {
    let Some(m) = model else {
        let url = if provider == "custom_pass" {
            base.to_owned()
        } else {
            format!("{base}/models")
        };
        return Ok((axum::http::Method::GET, url, bytes::Bytes::new()));
    };
    let (url, payload) = match provider {
        "anthropic" => (
            format!("{base}/v1/messages"),
            json!({"model": m, "max_tokens": PROBE_MAX_TOKENS,
                   "messages": [{"role": "user", "content": "ping"}]}),
        ),
        "gemini" => (
            format!("{base}/models/{m}:generateContent"),
            json!({"contents": [{"parts": [{"text": "ping"}]}],
                   "generationConfig": {"maxOutputTokens": PROBE_MAX_TOKENS}}),
        ),
        // custom_pass 是任意路径透传，没有「一次最小补全」的通用形状
        "custom_pass" => {
            return Err(AppError::bad_request().with_param("model_probe_unsupported"));
        }
        _ => (
            format!("{base}/chat/completions"),
            json!({"model": m, "max_tokens": PROBE_MAX_TOKENS,
                   "messages": [{"role": "user", "content": "ping"}]}),
        ),
    };
    Ok((
        axum::http::Method::POST,
        url,
        bytes::Bytes::from(serde_json::to_vec(&payload).unwrap_or_default()),
    ))
}

/// 渠道探测核心（REST 与 MCP channel_test 共用）。
///
/// `model = None`：只打 `/models`，验的是「凭证认不认、网络通不通」。
/// `model = Some(m)`：真发一次 1 token 的最小补全，验的是「**这个模型**调得通吗」。
///
/// 为什么要分两种：聚合型上游的凭证是全站有效的，但模型按套餐授权——只探 `/models`
/// 会对一个实际返回 403 的模型报 `ok: true`，把运维直接引到错误结论上（实测复现过）。
/// 模型探测会在上游真实产生一次调用（`max_tokens=1`），是管理员显式动作，不计站内账。
pub(crate) async fn probe_channel(
    state: &AppState,
    channel_id: i64,
    model: Option<&str>,
) -> Result<Value, AppError> {
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
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
    let base = row.api_base.unwrap_or_default();
    let base = base.trim_end_matches('/');

    // 按协议选探测端点与凭证头
    let (auth_header, auth_value) = match row.provider.as_str() {
        "anthropic" => ("x-api-key".to_owned(), credential),
        "gemini" => ("x-goog-api-key".to_owned(), credential),
        _ => ("authorization".to_owned(), format!("Bearer {credential}")),
    };
    let (method, url, body) = probe_request(&row.provider, base, model)?;
    let content_type = (!body.is_empty()).then(|| "application/json".to_owned());

    let started = std::time::Instant::now();
    let outcome = state
        .pass
        .forward(okapi_providers::custom_pass::PassRequest {
            method,
            url,
            auth_header,
            auth_value,
            content_type,
            body,
        })
        .await;
    let latency_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let at = chrono::Utc::now().to_rfc3339();
    // scope 让调用方一眼知道这个 ok 是什么意思：credential=只验了凭证与连通性，
    // model=这个模型真的调得通。少了它，两种探测的 `ok:true` 长得一模一样。
    let scope = if model.is_some() { "model" } else { "credential" };
    let result = match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { status, .. }) => {
            json!({"ok": true, "http_status": status, "latency_ms": latency_ms, "at": at,
                   "scope": scope, "model": model})
        }
        Ok(okapi_providers::custom_pass::PassResponse::ErrStatus { status, body }) => {
            // 失败带上游原文（截断）：403 到底是凭证问题还是模型没开通，全在这句话里
            let detail: String = String::from_utf8_lossy(&body).chars().take(300).collect();
            json!({"ok": false, "http_status": status, "latency_ms": latency_ms, "at": at,
                   "scope": scope, "model": model, "upstream_body": detail})
        }
        Err(err) => {
            json!({"ok": false, "error_code": err.error_code(), "latency_ms": latency_ms,
                   "at": at, "scope": scope, "model": model})
        }
    };
    // 留痕供列表页"最近测试"列回填（new-api 的 response_time/test_time 语义）
    state.sched.channel_test_record(channel_id, &result).await;
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
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
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

#[derive(Deserialize)]
pub struct RepairReq {
    /// 只修这一个用户。与 `all` 二选一。
    #[serde(default)]
    pub user_id: Option<i64>,
    /// 修当前扫出的全部漂移用户。
    #[serde(default)]
    pub all: bool,
    /// `all` 时的扫描上限（与对账视图同一口径）。
    #[serde(default = "default_recon_limit")]
    pub limit: i64,
}

const fn default_recon_limit() -> i64 {
    1000
}

/// POST /admin/reconciliation/repair：按账本重建热余额与展示快照。
///
/// 为什么需要：Redis 是唯一热账本且 `reserve.lua` 对缺键按余额 0 处理、不回源 PG，
/// 所以「Redis 活着但数据没了」（没开持久化重启 / 切到空副本 / maxmemory 淘汰 /
/// 手滑 FLUSHDB）会让全站付费请求静默拒服务，且**不会自愈**——对账任务此前只报不修，
/// 全仓也没有第二个入口能把余额写回去，站长只能眼看着差额挂在页面上。
///
/// 权威源是 `billing_events` 求和，操作幂等；在途预扣保持不动（`avail + 在途 == 账本`）。
pub async fn repair_reconciliation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RepairReq>,
) -> Result<Json<Value>, AppError> {
    // 它会改余额，权限跟充值/扣减同一道闸——虽然改的方向是"改回账本说的数"
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    let repaired = match (req.user_id, req.all) {
        (Some(user_id), false) => {
            // 先确认漂移是稳定的：结算是异步的，「Redis 已扣、PG 事件还没落」的中间态
            // 看起来和真丢数据一模一样，照着修等于把正在结算的那笔退回去
            match crate::worker::stable_drift(&state.pg, &state.ledger, user_id).await {
                Ok(None) => {
                    let exists = sqlx::query_scalar!(
                        r#"SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)
                           AS "e!""#,
                        user_id
                    )
                    .fetch_one(&state.pg)
                    .await
                    .map_err(okapi_store::StoreError::from)?;
                    return Err(if exists {
                        // 账目正在变动 → 让管理员过几秒重试，而不是拿一个半截的账本去覆写
                        AppError::new(StatusCode::CONFLICT, codes::RECONCILE_UNSTABLE)
                    } else {
                        AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
                    });
                }
                Err(err) => {
                    tracing::error!(error = %err, user_id, "对账稳定性判定失败");
                    return Err(AppError::internal());
                }
                Ok(Some(_)) => {}
            }
            match crate::worker::repair_balance(&state.pg, &state.ledger, user_id).await {
                Ok(Some(one)) => vec![one],
                Ok(None) => return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)),
                Err(err) => {
                    tracing::error!(error = %err, user_id, "对账修复失败");
                    return Err(AppError::internal());
                }
            }
        }
        (None, true) => crate::worker::repair_drifted(&state.pg, &state.ledger, req.limit)
            .await
            .map_err(|err| {
                tracing::error!(error = %err, "批量对账修复失败");
                AppError::internal()
            })?,
        // 既没指定用户又没给 all：批量改余额不该是手滑的默认行为
        _ => return Err(AppError::bad_request().with_param("user_id_or_all")),
    };
    audit(
        &state,
        &actor,
        "billing.reconcile_repair",
        &req.user_id.map_or_else(|| "all".to_owned(), |id| id.to_string()),
        json!({ "repaired": repaired.len(), "detail": repaired }),
    )
    .await;
    Ok(Json(
        json!({ "repaired": repaired.len(), "data": repaired }),
    ))
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
    // 带上用户名：漂移行只有 user_id 时，管理员还得去用户页反查一趟
    let ids: Vec<i64> = drifts.iter().map(|d| d.user_id).collect();
    let names: std::collections::HashMap<i64, String> =
        sqlx::query!(r#"SELECT id, username FROM users WHERE id = ANY($1)"#, &ids)
            .fetch_all(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .into_iter()
            .map(|r| (r.id, r.username))
            .collect();
    let data: Vec<Value> = drifts
        .iter()
        .map(|d| {
            json!({
                "user_id": d.user_id,
                "username": names.get(&d.user_id),
                "events_sum_micro": d.events_sum_micro,
                "redis_effective_micro": d.redis_effective_micro,
                "pg_snapshot_micro": d.pg_snapshot_micro,
            })
        })
        .collect();
    Ok(Json(json!({ "drift_count": data.len(), "drifts": data })))
}
