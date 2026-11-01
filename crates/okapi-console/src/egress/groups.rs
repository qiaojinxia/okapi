//! 代理分组、key 绑定、全局缺省出口与渠道出口。
use super::super::admin::{audit, ensure_channel_owner, guard_scoped};
use super::super::query::{PageQuery, Query};
use super::{
    ensure_group, ensure_name, ensure_note, ensure_proxy, forbidden_owner, not_found, owner_filter,
    validate_binding, visible,
};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_kernel::extract::Path;
use okapi_store::auth::PermScope;
use okapi_store::egress::{self, Binding, GroupMember};
use serde::Deserialize;
use serde_json::{Value, json};

// ---- 代理组 ----

pub(super) const GROUP_MODES: [&str; 2] = ["pinned", "rotate"];

pub(super) fn ensure_group_code(code: &str) -> Result<&str, AppError> {
    super::super::identifiers::ensure_code("code", code, 32)
}

/// GET /admin/proxy-groups。
pub async fn list_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let page = egress::list_groups(&state.pg, owner_filter(scope, &actor), q.slice()).await?;
    Ok(Json(json!({ "data": page.data, "total": page.total })))
}

#[derive(Deserialize)]
pub struct UpsertGroupReq {
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
    pub mode: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub members: Vec<GroupMember>,
}

/// POST /admin/proxy-groups：新建或覆盖（成员整组替换），同事务对账固定分配。
pub async fn upsert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpsertGroupReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let code = ensure_group_code(&req.code)?;
    if !GROUP_MODES.contains(&req.mode.as_str()) {
        return Err(AppError::bad_request().with_param("mode"));
    }
    let exists = egress::group_owner(&state.pg, code).await?;
    if let Some(owner) = exists
        && !visible(scope, &actor, owner)
    {
        return Err(forbidden_owner());
    }
    let name = ensure_name(req.name.as_deref().unwrap_or(code))?;
    let description = ensure_note(req.description.as_deref())?;
    let mut seen = std::collections::HashSet::new();
    for member in &req.members {
        if !seen.insert(member.proxy_id) {
            return Err(AppError::bad_request().with_param("members"));
        }
        if member.weight <= 0 || !(-1000..=1000).contains(&member.priority) {
            return Err(AppError::bad_request().with_param("members"));
        }
        ensure_proxy(&state, member.proxy_id, &actor, scope).await?;
    }
    let report = egress::upsert_group(
        &state.pg,
        &egress::GroupInput {
            code,
            name,
            mode: &req.mode,
            description,
            owner_id: Some(actor.actor_user_id()),
            members: &req.members,
        },
    )
    .await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "proxy_group.upsert",
        code,
        json!({ "mode": req.mode, "members": req.members, "assignment": report }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "assignment": report })))
}

/// DELETE /admin/proxy-groups/{code}。被渠道直接绑定 / 是全局默认 → 409。
pub async fn delete_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_group(&state, &code, &actor, scope).await?;
    if !egress::delete_group(&state.pg, &code).await? {
        return Err(not_found());
    }
    state.invalidate_routing_caches();
    audit(&state, &actor, "proxy_group.delete", &code, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

/// GET /admin/proxy-groups/{code}/assignments：有效出口是该组的全部 key 及其分配
/// （含经全局默认继承的），供「哪个账号在哪个 IP 上」一览与手动调整。
pub async fn group_assignments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    ensure_group(&state, &code, &actor, scope).await?;
    let mut rows = egress::group_assignments(&state.pg, &code).await?;
    if scope != PermScope::All {
        // own 范围：只列自己渠道下的 key
        let mine = okapi_store::admin::list_channels(
            &state.pg,
            okapi_store::admin::ChannelFilter {
                owner: Some(actor.actor_user_id()),
                ..Default::default()
            },
            okapi_store::listing::Slice::ALL,
        )
        .await?;
        let ids: std::collections::HashSet<i64> = mine.page.data.iter().map(|c| c.id).collect();
        rows.retain(|r| ids.contains(&r.channel_id));
    }
    Ok(Json(json!({ "data": rows })))
}

#[derive(Deserialize)]
pub struct AssignReq {
    pub key_id: i64,
    pub proxy_id: i64,
}

/// POST /admin/proxy-groups/{code}/assignments：把一把 key 手动改分到组内另一个代理。
pub async fn assign_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    ExtractJson(req): ExtractJson<AssignReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_group(&state, &code, &actor, scope).await?;
    ensure_proxy(&state, req.proxy_id, &actor, scope).await?;
    let channel_id: i64 = sqlx::query_scalar("SELECT channel_id FROM channel_keys WHERE id = $1")
        .bind(req.key_id)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(not_found)?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    egress::assign_key(&state.pg, req.key_id, req.proxy_id).await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "proxy_group.assign",
        &code,
        json!({ "key_id": req.key_id, "proxy_id": req.proxy_id }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 全局默认出口 ----

/// GET /admin/egress/default。
pub async fn get_default(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let binding = egress::default_binding(&state.pg).await?;
    Ok(Json(json!({ "egress": binding })))
}

/// PUT /admin/egress/default：继承默认的渠道随之换出口（固定分配同事务对账）。要求 all 范围。
pub async fn set_default(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(binding): ExtractJson<Binding>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    if scope != PermScope::All {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("egress_default_requires_all_scope"),
        );
    }
    if binding == Binding::Inherit {
        return Err(AppError::bad_request().with_param("mode"));
    }
    validate_binding(&state, &binding, &actor, scope).await?;
    let report = egress::set_default_binding(&state.pg, &binding, actor.actor_user_id()).await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "egress.set_default",
        egress::DEFAULT_SETTING,
        json!({ "egress": binding, "assignment": report }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "assignment": report })))
}

// ---- 渠道绑定 ----

/// POST /admin/channels/{id}/egress：`{"mode":"inherit"|"direct"|"proxy"|"group", ...}`。
pub async fn set_channel_egress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(binding): ExtractJson<Binding>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    validate_binding(&state, &binding, &actor, scope).await?;
    let report = egress::set_channel_binding(&state.pg, id, &binding)
        .await?
        .ok_or_else(not_found)?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.set_egress",
        &id.to_string(),
        json!({ "egress": binding, "assignment": report }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "assignment": report })))
}
