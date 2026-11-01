//! 定价分组、渠道池与用户分组归属。
use super::{RATIO_COLUMN_MAX_SCALED, audit, ensure_pool_scope, guard, guard_scoped};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_kernel::extract::Path;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

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
    /// 分组内每用户分钟 / 小时请求上限（§11.32）；null / 0 = 不限。
    #[serde(default)]
    pub rpm_limit: Option<i32>,
    #[serde(default)]
    pub rph_limit: Option<i32>,
}

/// 定价分组 upsert（改倍率后需 publish 生效；限额随鉴权缓存失效即时生效）。
pub async fn upsert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpsertGroupReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    // 分组码会拼进限流键与 `mb:blocks` 字段（`<group>|<channel_id>`），分隔符必须进不来
    let group_code = super::super::identifiers::ensure_code("group_code", &req.group_code, 32)?;
    let Some(ratio) = req
        .group_ratio
        .parse::<okapi_pricing::RatioFp>()
        .ok()
        .filter(|ratio| ratio.as_scaled() <= RATIO_COLUMN_MAX_SCALED)
    else {
        return Err(AppError::bad_request().with_param("group_ratio"));
    };
    // 0 与 null 同义（不限）；负数是笔误而不是"不限"，拒掉让人改
    let limit = |v: Option<i32>, param: &'static str| match v {
        Some(n) if n < 0 => Err(AppError::bad_request().with_param(param)),
        Some(0) | None => Ok(None),
        Some(n) => Ok(Some(n)),
    };
    let per_minute = limit(req.rpm_limit, "rpm_limit")?;
    let per_hour = limit(req.rph_limit, "rph_limit")?;
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
            group_code,
            group_ratio: &req.group_ratio,
            description: &req.description,
            pool_code: Some(pool_code),
            self_select: req.self_select,
            rpm_limit: per_minute,
            rph_limit: per_hour,
        },
    )
    .await?;
    // 分组的池 / 限额变了，绑定该组的 key 鉴权缓存里还是旧值——与改角色同一动作，全量失效
    state.sched.auth_flush().await;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "pricing.upsert_group",
        group_code,
        json!({ "group_ratio": req.group_ratio, "pool_code": pool_code, "self_select": req.self_select,
                "rpm_limit": per_minute, "rph_limit": per_hour }),
    )
    .await;
    // 池、限额、自选即时生效；新分组与倍率要等发布进价簿，告诉前端提示去发布
    let requires_publish = fresh_pricebook(&state)
        .await
        .group_ratio(&okapi_domain::GroupCode::from(group_code))
        != Some(ratio);
    Ok(Json(
        json!({ "ok": true, "requires_publish": requires_publish }),
    ))
}

/// 追到最新已发布的价簿再读：本进程价簿可能落后（console 只订阅广播、未配 NATS 时从不刷新），
/// 落后只会造成误拒，不会放进坏状态。
pub(super) async fn fresh_pricebook(state: &AppState) -> Arc<okapi_pricing::PriceBook> {
    if let Err(err) = okapi_gateway::refresh_pricebook_if_newer(state).await {
        tracing::warn!(error = %err, "刷新价簿失败，按当前价簿校验");
    }
    state.pricebook.load()
}

/// 分组存在且已发布进价簿。分组写进 `price_groups` 要等下一次发布才进价簿；把用户或 key 放进价簿
/// 还不认识的分组，之后每笔请求都会在计价处撞 `UnknownGroup`（fail-closed 回 500），直到有人发布。
pub(super) async fn ensure_group_usable(
    state: &AppState,
    group_code: &str,
    param: &'static str,
) -> Result<(), AppError> {
    // 直接撞 FK 只能回 500，拿不到可渲染的 error_code
    if !okapi_store::admin::price_group_exists(&state.pg, group_code).await? {
        return Err(AppError::bad_request().with_param(param));
    }
    if !fresh_pricebook(state)
        .await
        .has_group(&okapi_domain::GroupCode::from(group_code))
    {
        return Err(
            AppError::new(StatusCode::CONFLICT, codes::GROUP_NOT_PUBLISHED)
                .with_param(group_code.to_owned()),
        );
    }
    Ok(())
}

/// 池存在性校验：不存在回 404 带 param，而不是让外键违约变成 500。
pub(super) async fn ensure_pool_exists(state: &AppState, pool_code: &str) -> Result<(), AppError> {
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
pub(super) const ROUTING_STRATEGIES: &[&str] = &[
    "priority_weighted",
    "least_latency",
    "least_usage",
    "least_concurrency",
    "soonest_reset",
];

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
    ExtractJson(req): ExtractJson<UpsertPoolReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    // 池是全站共享的选路单元、没有属主：own 范围若能建/改池（改 routing_strategy、
    // 指定 fallback 链），等同于改写全站路由行为，与挂载渠道同级，一并只留给 all。
    ensure_pool_scope(scope)?;
    let strategy = req
        .routing_strategy
        .as_deref()
        .unwrap_or("priority_weighted");
    if !ROUTING_STRATEGIES.contains(&strategy) {
        return Err(AppError::bad_request().with_param("routing_strategy"));
    }
    let pool_code = super::super::identifiers::ensure_code("pool_code", &req.pool_code, 32)?;
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
    ExtractJson(req): ExtractJson<UserGroupsReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_MANAGE).await?;
    let groups: Vec<(String, i32)> = req
        .groups
        .iter()
        .map(|g| (g.group_code.clone(), g.priority))
        .collect();
    // 用户不存在（或已软删）→ 404，与改角色 / 改倍率一致；否则非空分组会撞 user_id 外键回 500。
    // 查询文本与 manage.rs 同一句逐字相同，复用 .sqlx 里已有的离线缓存条目。
    let live = sqlx::query_scalar!(
        r#"SELECT role FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if live.is_none() {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    // 同一分组列两次会撞 user_groups 主键回 500；优先级该取哪一个也说不清，直接拒
    let mut seen = std::collections::HashSet::new();
    if groups.iter().any(|(code, _)| !seen.insert(code.as_str())) {
        return Err(AppError::bad_request().with_param("groups.duplicate"));
    }
    // 写入前校验分组存在且已发布，把配置错误拦在这一步，而不是让它变成用户侧的 500
    for (code, _) in &groups {
        ensure_group_usable(&state, code, "group_code").await?;
    }
    // 绑在新可选范围之外的 key 同一事务里改回跟随用户分组（见 store 注释）
    let keys_reset = okapi_store::admin::set_user_groups(&state.pg, user_id, &groups).await?;
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "user.set_groups",
        &user_id.to_string(),
        json!({ "groups": groups.iter().map(|(c, p)| json!({"code": c, "priority": p})).collect::<Vec<_>>(),
                "keys_reset": keys_reset }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "keys_reset": keys_reset })))
}
