//! 规则集的草稿、版本历史、发布与回滚（设计见 docs/compat-rules.md §9、§17.4）。
//!
//! 三类范围共用一套接口，路径里的 `{scope}` 是 `channel` / `profile` / `guard`（护栏的 `{id}` 写 0）：
//!
//! - `GET    /admin/compat/sets/{scope}/{id}`：已发布版本、草稿，以及草稿是否过期；
//! - `GET    /admin/compat/sets/{scope}/{id}/versions`、`.../versions/{version}`：版本历史与某一版的内容；
//! - `PUT    /admin/compat/sets/{scope}/{id}/draft`、`DELETE ...`：存 / 丢弃草稿（AI 经 MCP 也走这里的存储）；
//! - `POST   /admin/compat/sets/{scope}/{id}/draft/publish`：批准草稿，需要 `compat.publish`；
//! - `POST   /admin/compat/sets/{scope}/{id}/rollback`：把旧版本的内容作为新版本重新发布，需要 `compat.publish`；
//! - `PUT    /admin/compat/sets/{scope}/{id}/rules`：不经草稿直接保存即发布（渠道层与此前一致）；
//! - `GET    /admin/compat/drafts`：待批准的草稿收件箱（需要 `compat.publish`）。
//!
//! 谁能做什么：渠道层沿用 `channel.read` / `channel.write`（含 own / all 范围）；档案要 `compat.profile.write`；
//! 护栏要 `compat.guard.write`；发布草稿与回滚在此之上都要 `compat.publish`。批准的人如果没有
//! `compat.sensitive`，草稿里新增的敏感写入照样过不了（权限只看相对当前已发布版本**新增**的部分）。

use super::admin::{audit, ensure_channel_owner, guard};
use super::compat::{
    PreparedRules, RulesInput, check_rules, invalid_rules, note_of, prepare_rules, publish_rules,
    row_json,
};
use super::query::Query;
use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::extract::{Json as ExtractJson, Path};
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_store::AuthedKey;
use okapi_store::auth::PermScope;
use okapi_store::compat::{self, DraftInput, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// 规则集的归属（接口里的 `{scope}/{id}`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Channel(i64),
    Profile(i64),
    Guard,
}

impl Target {
    pub(super) fn parse(scope: &str, id: i64) -> Result<Self, AppError> {
        match scope {
            "channel" if id > 0 => Ok(Self::Channel(id)),
            "profile" if id > 0 => Ok(Self::Profile(id)),
            "guard" => Ok(Self::Guard),
            _ => Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)),
        }
    }

    pub(super) const fn scope(self) -> Scope {
        match self {
            Self::Channel(id) => Scope::Channel(id),
            Self::Profile(id) => Scope::Profile(id),
            Self::Guard => Scope::Global,
        }
    }

    /// 审计里的目标标识。
    pub(super) fn label(self) -> String {
        match self {
            Self::Channel(id) => format!("channel:{id}"),
            Self::Profile(id) => format!("profile:{id}"),
            Self::Guard => "guard".to_owned(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Access {
    /// 看。
    Read,
    /// 存 / 丢弃草稿。
    Draft,
    /// 批准草稿、回滚（在写权限之上还要 `compat.publish`）。
    Publish,
    /// 不经草稿直接保存即发布。
    Direct,
}

fn denied(permission: &str) -> AppError {
    AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED).with_param(permission.to_owned())
}

/// 这个目标上的这类操作要哪个权限点（渠道的 own / all 范围另算）。
fn required(target: Target, access: Access) -> &'static str {
    match (target, access) {
        (_, Access::Read) => permissions::CHANNEL_READ,
        (Target::Channel(_), _) => permissions::CHANNEL_WRITE,
        (Target::Profile(_), _) => permissions::COMPAT_PROFILE_WRITE,
        (Target::Guard, _) => permissions::COMPAT_GUARD_WRITE,
    }
}

/// 鉴权：目标不存在回 404，没有权限回 403。
pub(super) async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    target: Target,
    access: Access,
) -> Result<Arc<AuthedKey>, AppError> {
    let actor = authenticate(state, headers).await?;
    check(state, &actor, target, access).await?;
    super::audit_fallback::note_actor(actor.actor_user_id(), required(target, access));
    Ok(actor)
}

/// [`authorize`] 的后半：`actor` 已经认证过（REST 的登录态，或 MCP 的 key），查权限点、行级范围与存在性。
pub(super) async fn check(
    state: &AppState,
    actor: &AuthedKey,
    target: Target,
    access: Access,
) -> Result<(), AppError> {
    let permission = required(target, access);
    let scope = actor.permission_scope(permission);
    // 渠道层与只读接受 own 范围（属主另查）；档案 / 护栏的写权限点没有 own 之分，要全量
    let granted = if matches!(target, Target::Channel(_)) || access == Access::Read {
        scope != PermScope::Denied
    } else {
        scope == PermScope::All
    };
    if !granted {
        return Err(denied(permission));
    }
    if let Target::Channel(id) = target {
        ensure_channel_owner(state, id, actor, scope).await?;
    }
    // 批准草稿 / 回滚总要 compat.publish；档案不经草稿直接发布同样要（它一改影响所有选了它的渠道）
    let needs_publish = access == Access::Publish
        || (access == Access::Direct && matches!(target, Target::Profile(_)));
    if needs_publish && !actor.has_permission(permissions::COMPAT_PUBLISH) {
        return Err(denied(permissions::COMPAT_PUBLISH));
    }
    let exists: bool = match target {
        Target::Channel(id) => {
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM channels WHERE id = $1 AND deleted_at IS NULL)",
            )
            .bind(id)
            .fetch_one(&state.pg)
            .await
        }
        Target::Profile(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM compat_profiles WHERE id = $1)")
                .bind(id)
                .fetch_one(&state.pg)
                .await
        }
        Target::Guard => Ok(true),
    }
    .map_err(okapi_store::StoreError::from)?;
    if exists {
        Ok(())
    } else {
        Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))
    }
}

fn not_found() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
}

/// GET /admin/compat/sets/{scope}/{id}
pub async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id)): Path<(String, i64)>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    authorize(&state, &headers, target, Access::Read).await?;
    let published = compat::published(&state.pg, target.scope()).await?;
    let draft = compat::draft(&state.pg, target.scope()).await?;
    // 草稿起草时的已发布版本与现在对不上 = 过期：批准前要重新起草
    let outdated = draft.as_ref().is_some_and(|draft| {
        draft.base_version.unwrap_or(0) != published.as_ref().map_or(0, |row| row.version)
    });
    Ok(Json(json!({
        "scope": scope, "id": id,
        "published": published.as_ref().map(row_json),
        "draft": draft.as_ref().map(row_json),
        "stale": outdated,
    })))
}

#[derive(Debug, Deserialize)]
pub struct VersionsQuery {
    pub limit: Option<i64>,
}

/// GET /admin/compat/sets/{scope}/{id}/versions：版本历史（不带规则内容，只带条数）。
pub async fn versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id)): Path<(String, i64)>,
    Query(query): Query<VersionsQuery>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    authorize(&state, &headers, target, Access::Read).await?;
    let rows = compat::versions(&state.pg, target.scope(), query.limit.unwrap_or(50)).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut item = row_json(row);
            item["rules"] = json!(row.rules.as_array().map_or(0, Vec::len));
            item
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

/// GET /admin/compat/sets/{scope}/{id}/versions/{version}：某一版的完整内容。
pub async fn version_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id, version)): Path<(String, i64, i32)>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    authorize(&state, &headers, target, Access::Read).await?;
    let row = compat::version(&state.pg, target.scope(), version)
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(row_json(&row)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftReq {
    pub rules: Value,
    /// 起草时读到的已发布版本（没有 = 0）；缺省按现在的已发布版本算。对不上回 409。
    #[serde(default)]
    pub base_version: Option<i32>,
    #[serde(default)]
    pub note: Option<String>,
    /// 为什么要这么改（给批准的人看）。
    #[serde(default)]
    pub rationale: Option<String>,
}

/// 依据：去空白，空串当没有，最长 1000 字符（列宽），不含控制字符（换行除外）。
fn rationale_of(raw: Option<&str>) -> Result<Option<&str>, AppError> {
    let text = raw.map(str::trim).filter(|t| !t.is_empty());
    if text
        .is_some_and(|t| t.chars().count() > 1000 || t.chars().any(|c| c.is_control() && c != '\n'))
    {
        return Err(AppError::bad_request().with_param("rationale"));
    }
    Ok(text)
}

/// 一份要存的草稿。
pub(super) struct DraftSpec<'a> {
    pub rules: &'a Value,
    /// 起草时读到的已发布版本；缺省按现在的算。
    pub base_version: Option<i32>,
    pub note: Option<&'a str>,
    pub rationale: Option<&'a str>,
    /// `admin` 或 `mcp`。
    pub source: &'a str,
}

/// 存草稿的共用实现（REST 与 MCP 的 `compat_propose` 走同一处）。返回草稿版本号与起草所依据的已发布版本；
/// 审计由调用方写（REST 记成管理员，MCP 记成那把 key）。
pub(super) async fn store_draft(
    state: &AppState,
    actor: &AuthedKey,
    target: Target,
    spec: DraftSpec<'_>,
) -> Result<(i32, i32), AppError> {
    let DraftSpec {
        rules,
        base_version,
        note,
        rationale,
        source,
    } = spec;
    let note = note_of(note)?;
    let rationale = rationale_of(rationale)?;
    let valid = okapi_compat::validate(rules).map_err(|issues| invalid_rules(&issues))?;
    let normalized = valid.to_value();
    let current = compat::published(&state.pg, target.scope())
        .await?
        .map_or(0, |row| row.version);
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let base = base_version.unwrap_or(current);
    let version = compat::save_draft(
        &mut tx,
        target.scope(),
        DraftInput {
            rules: &normalized,
            note,
            rationale,
            actor: Some(actor.actor_user_id()),
            source,
            base_version: base,
        },
    )
    .await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    Ok((version, base))
}

/// PUT /admin/compat/sets/{scope}/{id}/draft
pub async fn save_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id)): Path<(String, i64)>,
    ExtractJson(req): ExtractJson<DraftReq>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    let actor = authorize(&state, &headers, target, Access::Draft).await?;
    let (version, base) = store_draft(
        &state,
        &actor,
        target,
        DraftSpec {
            rules: &req.rules,
            base_version: req.base_version,
            note: req.note.as_deref(),
            rationale: req.rationale.as_deref(),
            source: "admin",
        },
    )
    .await?;
    audit(
        &state,
        &actor,
        "compat.draft.save",
        &target.label(),
        json!({"version": version, "source": "admin", "base_version": base}),
    )
    .await;
    Ok(Json(json!({ "ok": true, "version": version })))
}

/// DELETE /admin/compat/sets/{scope}/{id}/draft
pub async fn discard_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id)): Path<(String, i64)>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    let actor = authorize(&state, &headers, target, Access::Draft).await?;
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let discarded = compat::discard_draft(&mut tx, target.scope()).await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    if !discarded {
        return Err(not_found());
    }
    audit(
        &state,
        &actor,
        "compat.draft.discard",
        &target.label(),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// POST /admin/compat/sets/{scope}/{id}/draft/publish
pub async fn publish_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id)): Path<(String, i64)>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    let actor = authorize(&state, &headers, target, Access::Publish).await?;
    let draft = compat::draft(&state.pg, target.scope())
        .await?
        .ok_or_else(not_found)?;
    let published = compat::published(&state.pg, target.scope()).await?;
    // 草稿落库后引擎可能升级过：发布前再校验一遍；敏感权限只看相对线上版本新增的部分
    let prepared = check_rules(
        &actor,
        &draft.rules,
        published.as_ref().map(|row| &row.rules),
    )?;
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let version = compat::publish_draft(&mut tx, target.scope(), Some(actor.actor_user_id()))
        .await?
        .ok_or_else(not_found)?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "compat.draft.publish",
        &target.label(),
        json!({
            "version": version, "rules": prepared.count, "source": draft.source,
            "drafted_by": draft.created_by, "sensitive_added": prepared.sensitive_added,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "version": version })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackReq {
    pub version: i32,
}

/// POST /admin/compat/sets/{scope}/{id}/rollback
pub async fn rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id)): Path<(String, i64)>,
    ExtractJson(req): ExtractJson<RollbackReq>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    let actor = authorize(&state, &headers, target, Access::Publish).await?;
    let old = compat::version(&state.pg, target.scope(), req.version)
        .await?
        .filter(|row| row.status != "draft")
        .ok_or_else(not_found)?;
    let published = compat::published(&state.pg, target.scope()).await?;
    let prepared: PreparedRules =
        check_rules(&actor, &old.rules, published.as_ref().map(|row| &row.rules))?;
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let version = compat::rollback(
        &mut tx,
        target.scope(),
        req.version,
        Some(actor.actor_user_id()),
    )
    .await?
    .ok_or_else(not_found)?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "compat.rollback",
        &target.label(),
        json!({"to_version": req.version, "version": version, "sensitive_added": prepared.sensitive_added}),
    )
    .await;
    Ok(Json(json!({ "ok": true, "version": version })))
}

/// PUT /admin/compat/sets/{scope}/{id}/rules：不经草稿，直接保存即发布。
pub async fn publish_direct(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((scope, id)): Path<(String, i64)>,
    ExtractJson(input): ExtractJson<RulesInput>,
) -> Result<Json<Value>, AppError> {
    let target = Target::parse(&scope, id)?;
    let actor = authorize(&state, &headers, target, Access::Direct).await?;
    super::compat::ensure_base_version(&state, target.scope(), input.base_version).await?;
    let prepared = prepare_rules(&state, &actor, Some(target.scope()), &input).await?;
    let version = publish_rules(&state, &actor, target.scope(), &prepared, &input).await?;
    audit(
        &state,
        &actor,
        "compat.rules.publish",
        &target.label(),
        json!({
            "version": version, "rules": prepared.count,
            "sensitive_added": prepared.sensitive_added,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "version": version })))
}

type InboxRow = (
    String,
    Option<i64>,
    i32,
    Option<i32>,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    chrono::DateTime<chrono::Utc>,
    i32,
    Option<String>,
    Option<i32>,
);

/// GET /admin/compat/drafts：待批准的草稿（所有范围），最新的在前。
pub async fn inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::COMPAT_PUBLISH).await?;
    let rows: Vec<InboxRow> = sqlx::query_as(
        "SELECT r.scope_kind, r.scope_id, r.version, r.base_version, r.source, r.rationale, r.note,
                r.created_by, r.created_at, jsonb_array_length(r.rules)::int,
                COALESCE(c.name, p.name),
                (SELECT x.version FROM rule_sets x
                  WHERE x.kind = r.kind AND x.scope_kind = r.scope_kind
                    AND x.scope_id IS NOT DISTINCT FROM r.scope_id AND x.status = 'published')
           FROM rule_sets r
           LEFT JOIN channels c ON r.scope_kind = 'channel' AND c.id = r.scope_id
           LEFT JOIN compat_profiles p ON r.scope_kind = 'profile' AND p.id = r.scope_id
          WHERE r.kind = 'compat' AND r.status = 'draft'
          ORDER BY r.created_at DESC
          LIMIT 200",
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                scope,
                scope_id,
                version,
                base,
                source,
                rationale,
                note,
                by,
                at,
                count,
                name,
                live,
            )| {
                json!({
                    "scope": if scope == "global" { "guard" } else { scope.as_str() },
                    "id": scope_id.unwrap_or(0), "name": name,
                    "version": version, "base_version": base, "published_version": live,
                    "stale": base.unwrap_or(0) != live.unwrap_or(0),
                    "source": source, "rationale": rationale, "note": note,
                    "created_by": by, "created_at": at, "rules": count,
                })
            },
        )
        .collect();
    Ok(Json(json!({ "data": data })))
}
