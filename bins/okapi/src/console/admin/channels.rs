//! 渠道管理：创建 / 列表 / 改 / 删、启停与池归属、key 调度参数、凭证轮换、用量。
use super::super::query::{PageQuery, Query};
use super::{
    audit, ensure_account_control, ensure_azure_api_base, ensure_channel_owner,
    ensure_request_extensions, ensure_settings_api_version, ensure_settings_mimic,
    ensure_settings_oauth_token_url, ensure_settings_outbound, ensure_settings_tool_loss_policy,
    guard_scoped,
};
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_store::auth::PermScope;
use serde::Deserialize;
use serde_json::{Value, json};

// ---- 渠道 ----

#[derive(Deserialize)]
pub struct CreateChannelReq {
    pub name: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub api_base: String,
    pub credential: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub trust_upstream_usage: bool,
    /// The same optional settings are accepted by subscription authorization.
    #[serde(flatten)]
    pub options: super::super::channel_creation::Options,
}

/// 相对成本系数取值域：0（免费）～ 100×官方价，负数与离谱值都是手滑。
pub(in crate::console) fn ensure_cost_milli(value: Option<i64>) -> Result<(), AppError> {
    match value {
        Some(v) if !(0..=100_000).contains(&v) => {
            Err(AppError::bad_request().with_param("cost_milli"))
        }
        _ => Ok(()),
    }
}

pub(in crate::console) fn ensure_max_concurrency(value: Option<i32>) -> Result<(), AppError> {
    if value.is_some_and(|cap| cap <= 0) {
        return Err(AppError::bad_request().with_param("max_concurrency"));
    }
    Ok(())
}

pub(super) fn default_provider() -> String {
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
/// 池成员关系只有 all 范围能写。池是全站共享的选路单元、没有属主：own 范围管的是"自己的渠道"，
/// 若能把它挂进 default（或付费分组的池、带最高优先级覆盖），几秒后全站请求就路由到它的
/// api_base——prompt 与响应全量外泄。own 范围建的渠道因此是孤儿，由 all 范围管理员审过再入池。
pub(in crate::console) fn ensure_pool_scope(scope: PermScope) -> Result<(), AppError> {
    if scope == PermScope::All {
        Ok(())
    } else {
        Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("pools_require_all_scope"),
        )
    }
}

pub(in crate::console) async fn normalize_members(
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

/// 渠道的模型清单入库前规整：每项去首尾空白、空项拒绝（400 `models`）、重复项去掉（保持首次出现的顺序）。
/// 带空白的模型名永远匹配不上请求里的名字，渠道看起来配好了却不接这个模型。
pub(in crate::console) fn normalize_models(raw: &[String]) -> Result<Vec<String>, AppError> {
    let mut models: Vec<String> = Vec::with_capacity(raw.len());
    for model in raw {
        let model = model.trim();
        if model.is_empty() {
            return Err(AppError::bad_request().with_param("models"));
        }
        if !models.iter().any(|known| known == model) {
            models.push(model.to_owned());
        }
    }
    Ok(models)
}

pub async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<CreateChannelReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    if okapi_providers::registry::lookup(&req.provider).is_none() {
        return Err(AppError::bad_request().with_param("provider"));
    }
    let api_base =
        super::super::channel_creation::endpoint(&state, &req.provider, Some(&req.api_base))
            .await?;
    ensure_azure_api_base(&req.provider, Some(api_base))?;
    super::super::ssrf::validate_credential(&state, &req.credential).await?;
    let prepared =
        super::super::channel_creation::prepare(&state, &req.provider, req.options, scope).await?;
    super::super::egress::validate_binding(&state, &prepared.egress, &actor, scope).await?;
    let credential = super::super::channel_credentials::normalize(&req.provider, &req.credential)?;
    let name = super::super::identifiers::ensure_display_name("name", &req.name, 128)?;
    let models = normalize_models(&req.models)?;
    if models.is_empty() {
        return Err(AppError::bad_request().with_param("models"));
    }
    let models: Vec<&str> = models.iter().map(String::as_str).collect();
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel_configured(
        &state.pg,
        okapi_store::provision::ChannelCreate {
            name,
            provider: &req.provider,
            api_base,
            credential: &credential,
            models: &models,
            trust_upstream_usage: req.trust_upstream_usage,
            owner_id: Some(actor.actor_user_id()),
            settings: Some(&prepared.settings),
            priority: prepared.priority,
            max_concurrency: prepared.max_concurrency,
            cost_milli: prepared.cost_milli,
            pools: Some(&prepared.pools),
            egress: Some(&prepared.egress),
            egress_preassigned: None,
        },
        state.master_key.as_deref(),
    )
    .await?;
    let pool_codes: Vec<&str> = prepared
        .pools
        .iter()
        .map(|member| member.pool_code.as_str())
        .collect();
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.create",
        &channel_id.to_string(),
        json!({ "name": name, "provider": req.provider, "models": models, "pools": pool_codes,
                "egress": prepared.egress }),
    )
    .await;
    Ok(Json(
        json!({ "channel_id": channel_id, "channel_key_id": channel_key_id }),
    ))
}

/// GET /admin/channels：`?q=`（名称 / 地址）、`provider`、`status` 过滤，`limit/offset` 切片；
/// 默认每页 20 条；需要全部渠道的调用方按 total 逐页读取。
/// 订阅 OAuth key（§11.38）的列表附加字段：access token 到期、刷新健康度、能否刷新、
/// 账号邮箱、订阅档位。只对 credential_kind=1 解密。
pub(super) async fn oauth_key_extras(
    state: &AppState,
    keys: &[okapi_store::admin::ChannelKeyRow],
    providers: &std::collections::HashMap<i64, &str>,
) -> std::collections::HashMap<i64, serde_json::Map<String, Value>> {
    let mut extras = std::collections::HashMap::new();
    for k in keys
        .iter()
        .filter(|k| k.credential_kind == okapi_store::admin::CREDENTIAL_KIND_OAUTH)
    {
        let mut fields = serde_json::Map::new();
        if let Some(health) = crate::gateway::credentials::health::read(state, k.id).await {
            fields.insert(
                "oauth_refresh".into(),
                serde_json::to_value(health).unwrap_or_default(),
            );
        }
        if let Ok(Some(plain)) =
            okapi_store::admin::read_key_credential(&state.pg, k.id, state.master_key.as_deref())
                .await
            && let Some(cred) = okapi_store::credential::OAuthCredential::parse(&plain)
        {
            if cred.expires_at > 0 {
                fields.insert("credential_expires_at".into(), json!(cred.expires_at));
            }
            fields.insert("oauth_refreshable".into(), json!(cred.can_refresh()));
            if let Some(label) = cred.account_label {
                fields.insert("account_label".into(), json!(label));
            }
            if let Some(provider) = providers.get(&k.channel_id)
                && let Some(plan) =
                    crate::gateway::account_control::quota::plan(state, k.id, provider, &plain)
                        .await
            {
                fields.insert("account_plan".into(), json!(plan));
            }
        }
        extras.insert(k.id, fields);
    }
    extras
}

pub async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let owner_filter = match scope {
        PermScope::Own => Some(actor.actor_user_id()),
        PermScope::All | PermScope::Denied => None,
    };
    let list = okapi_store::admin::list_channels(
        &state.pg,
        okapi_store::admin::ChannelFilter {
            owner: owner_filter,
            query: q.keyword(),
            provider: q.provider(),
            status: q.status,
        },
        q.slice(),
    )
    .await?;
    // key 与最近测活结果都只取本页渠道：渠道已切片，附属数据再整表拉就白分页了
    let ids: Vec<i64> = list.page.data.iter().map(|c| c.id).collect();
    let keys = okapi_store::admin::list_channel_keys_for(&state.pg, &ids).await?;
    let providers: std::collections::HashMap<i64, &str> = list
        .page
        .data
        .iter()
        .map(|c| (c.id, c.provider.as_str()))
        .collect();
    let mut oauth_extras = oauth_key_extras(&state, &keys, &providers).await;
    // 最近测活 / 余额查询结果各一次 MGET 回填（Redis 30 天 TTL；没测过 / 已过期 = null）
    let mut last_tests = state.sched.channel_test_get_many(&ids).await;
    let mut last_balances = state.sched.channel_balance_get_many(&ids).await;
    let data: Vec<Value> = list
        .page
        .data
        .into_iter()
        .map(|c| {
            let keys: Vec<Value> = keys
                .iter()
                .filter(|k| k.channel_id == c.id)
                .map(|k| {
                    let mut v = serde_json::to_value(k).unwrap_or_default();
                    if let (Some(extras), Some(obj)) =
                        (oauth_extras.remove(&k.id), v.as_object_mut())
                    {
                        obj.extend(extras);
                    }
                    v
                })
                .collect();
            json!({
                "id": c.id, "name": c.name, "provider": c.provider,
                "api_base": c.api_base, "status": c.status, "priority": c.priority,
                "models": c.models, "trust_upstream_usage": c.trust_upstream_usage,
                // 列表也带池：空数组 = 孤儿渠道（对谁都不可达），列表页要能一眼看见
                "pools": c.pools,
                "pool_members": c.pool_members,
                "cost_milli": c.cost_milli,
                // 编辑抽屉以列表行为初始值，保存时整体回写 settings；缺了它，改任意字段
                // 都会把请求风格、账号控制、代理等高级设置一并清空。
                "settings": c.settings,
                // 出口绑定（§11.41）；keys[].egress_proxy_id 是固定分配的结果
                "egress": okapi_store::egress::Binding::from_columns(
                    c.egress_mode.as_deref(),
                    c.egress_proxy_id,
                    c.egress_group_code.clone(),
                ),
                "keys": keys,
                "last_test": last_tests.remove(&c.id),
                "last_balance": last_balances.remove(&c.id),
            })
        })
        .collect();
    Ok(Json(
        json!({ "data": data, "total": list.page.total, "enabled": list.enabled }),
    ))
}

#[derive(Deserialize)]
pub struct SetStatusReq {
    pub status: i16,
}

pub async fn set_channel_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(req): ExtractJson<SetStatusReq>,
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
    ExtractJson(req): ExtractJson<SetPoolsReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    ensure_pool_scope(scope)?;
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

/// Public shape of compiled account hooks; no credentials or channel data.
pub async fn channel_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let data: Vec<Value> = okapi_providers::registry::BUILT_INS
        .iter()
        .map(|adapter| json!({"id": adapter.id, "default_base": adapter.default_base, "account": adapter.account_capabilities()}))
        .collect();
    Ok(Json(json!({"data": data})))
}

#[derive(Deserialize)]
pub struct ChannelUsageQuery {
    token_period: Option<crate::gateway::account_control::policy::TokenPeriod>,
}

pub async fn channel_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel): Path<i64>,
    Query(query): Query<ChannelUsageQuery>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    ensure_channel_owner(&state, channel, &actor, scope).await?;
    let settings: Value =
        sqlx::query_scalar("SELECT settings FROM channels WHERE id=$1 AND deleted_at IS NULL")
            .bind(channel)
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    let policy = crate::gateway::account_control::policy::Policy::parse(&settings)
        .map_err(|param| AppError::bad_request().with_param(param))?;
    let token_usage = okapi_store::channel_usage::token_snapshot(
        &state.pg,
        channel,
        query.token_period.map_or_else(
            || {
                policy
                    .local_tokens
                    .as_ref()
                    .map_or("total", |limit| limit.period.name())
            },
            |period| period.name(),
        ),
    )
    .await?;
    let keys: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM channel_keys WHERE channel_id=$1 ORDER BY id")
            .bind(channel)
            .fetch_all(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    let mut quotas = Vec::new();
    for key in keys {
        quotas.push(json!({"key_id":key,"quota":crate::gateway::account_control::quota::read(&state,key).await}));
    }
    Ok(Json(
        json!({"policy":policy,"quotas":quotas,"token_usage":token_usage,"timezone":okapi_store::timezone::machine_timezone()?}),
    ))
}

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
}

/// PATCH /admin/channels/{id}：改渠道配置（缺省字段不动）。
/// 启停仍走 `/status`，凭证走 `/credential`——三者审计语义不同，不合并。
#[allow(clippy::too_many_lines)]
pub async fn update_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(mut req): ExtractJson<PatchChannelReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    // 校验过的地址和落库的是同一个字符串：URL 解析会吞掉首尾空白，原样入库的空白却会拼进请求路径
    req.api_base = req.api_base.map(|base| base.trim().to_owned());

    if let Some(provider) = &req.provider
        && okapi_providers::registry::lookup(provider).is_none()
    {
        return Err(AppError::bad_request().with_param("provider"));
    }
    if let Some(api_base) = &req.api_base {
        super::super::ssrf::validate_upstream_base(&state, api_base).await?;
    }
    ensure_settings_api_version(req.settings.as_ref())?;
    ensure_settings_outbound(req.settings.as_ref())?;
    ensure_settings_oauth_token_url(&state, req.settings.as_ref()).await?;
    ensure_settings_mimic(req.settings.as_ref())?;
    ensure_settings_tool_loss_policy(req.settings.as_ref())?;
    ensure_account_control(req.settings.as_ref())?;
    if req.provider.is_some() || req.settings.is_some() {
        let (current_provider, current_settings): (String, Value) = sqlx::query_as(
            "SELECT provider, settings FROM channels WHERE id=$1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
        ensure_request_extensions(
            req.provider.as_deref().unwrap_or(&current_provider),
            Some(req.settings.as_ref().unwrap_or(&current_settings)),
        )?;
    }

    // 空地址已被上面的 SSRF 校验拦下（scheme 不合法）。要地址的三家（azure / bedrock / vertex）：
    // 本次给了地址就按新协议校验形状；改协议但没给地址则回源看现有地址合不合新协议
    if let Some(provider) = req
        .provider
        .as_deref()
        .filter(|p| matches!(*p, "azure" | "bedrock" | "vertex"))
    {
        if req.api_base.is_some() {
            ensure_azure_api_base(provider, req.api_base.as_deref())?;
        } else {
            let current = sqlx::query_scalar!(
                r#"SELECT api_base FROM channels WHERE id = $1 AND deleted_at IS NULL"#,
                id
            )
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, okapi_api::codes::NOT_FOUND))?;
            ensure_azure_api_base(provider, current.as_deref())?;
        }
    } else if req.provider.is_none()
        && let Some(api_base) = req.api_base.as_deref()
    {
        // 只改地址不改协议：vertex 渠道的新地址仍要满足形状（其它协议无形状约束）
        let provider = sqlx::query_scalar!(
            r#"SELECT provider FROM channels WHERE id = $1 AND deleted_at IS NULL"#,
            id
        )
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, okapi_api::codes::NOT_FOUND))?;
        if provider == "vertex" {
            ensure_azure_api_base("vertex", Some(api_base))?;
        }
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
    let models = req
        .models
        .as_deref()
        .map(normalize_models)
        .transpose()?
        .map(|list| json!(list));
    ensure_cost_milli(req.cost_milli)?;

    let name = req
        .name
        .as_deref()
        .map(|name| super::super::identifiers::ensure_display_name("name", name, 128))
        .transpose()?;
    let patch = okapi_store::admin::ChannelPatch {
        name,
        provider: req.provider.as_deref(),
        api_base: req.api_base.as_deref(),
        models: models.as_ref(),
        model_mapping: req.model_mapping.as_ref(),
        settings: req.settings.as_ref(),
        capabilities: req.capabilities.as_ref(),
        priority: req.priority,
        trust_upstream_usage: req.trust_upstream_usage,
        cost_milli: req.cost_milli,
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
            "name": name, "provider": req.provider, "api_base": req.api_base,
            "models": req.models, "priority": req.priority,
            "trust_upstream_usage": req.trust_upstream_usage,
            "cost_milli": req.cost_milli,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// DELETE /admin/channels/{id}：软删除并停用其下 key（历史账单按 channel_id 仍可回溯），与批量删除同口径。
pub async fn delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    if !okapi_store::mutate::delete_channel(&state.pg, id).await? {
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
    ExtractJson(req): ExtractJson<RotateCredentialReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    if req.credential.trim().is_empty() {
        return Err(AppError::bad_request().with_param("credential"));
    }
    super::super::ssrf::validate_credential(&state, &req.credential).await?;
    let provider: String =
        sqlx::query_scalar("SELECT provider FROM channels WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    let credential =
        super::super::channel_credentials::normalize(&provider, req.credential.trim())?;
    let outcome = okapi_store::admin::rotate_channel_credential(
        &state.pg,
        id,
        req.channel_key_id,
        &credential,
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
    #[serde(default, deserialize_with = "super::super::double_option")]
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
    ExtractJson(req): ExtractJson<PatchChannelKeyReq>,
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
