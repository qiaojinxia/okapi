//! 适配规则管理接口（设计见 docs/compat-rules.md §9、§12、§17）。
//!
//! - `GET  /admin/channels/{id}/compat`：厂商 / 档案、渠道层规则与草稿，以及合并后的「有效规则」（含来源与被盖住的）；
//! - `PUT  /admin/channels/{id}/compat`：保存厂商 / 渠道层规则（新版本 + 发布，带 `base_version` 防互相覆盖）；
//! - `POST /admin/compat/dry-run`：四个阶段都能在样本上试跑，与线上走同一份引擎代码；
//! - `GET  /admin/compat/vendors`：内置厂商目录与适配档案，附按主机的建议（只建议，不静默应用）；
//! - `GET  /admin/compat/catalog`：规则的自描述目录（蓝图 / 表单 / MCP 共用）；
//! - `GET / PUT /admin/compat/guard`：全局护栏（写需要 `compat.guard.write`）。
//!
//! 草稿、版本历史、发布与回滚在 [`super::compat_sets`]，适配档案与解释接口在 [`super::compat_profiles`]。
//!
//! 权限：渠道层沿用 `channel.read` / `channel.write`（含 own / all 范围）；新增写入提示词 / 指令 /
//! 档位等敏感路径的操作（响应里还包括动用量 / 计价事实）需要 `compat.sensitive`（只是删字段不需要）；
//! 护栏影响所有渠道，所以单独一个权限点。直接保存即发布保持不变；草稿走审批（`compat.publish`）。

use super::admin::{audit, ensure_channel_owner, guard, guard_scoped};
use super::query::Query;
use crate::gateway::error::AppError;
use crate::gateway::extract::{Json as ExtractJson, Path};
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_compat::{Action, ContextInput, Issue, Phase, vendors};
use okapi_store::AuthedKey;
use okapi_store::auth::PermScope;
use okapi_store::compat::{self, ChannelLayer, Layers, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

/// 规则集不合法。`param` 带第一个问题的「规则 id:问题码」，完整问题列表走 dry-run。
pub(super) const INVALID_RULES: &str = "compat_rules_invalid";

/// 创建 / 修改渠道时随体带来的渠道层规则。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesInput {
    pub rules: Value,
    /// 调用方读到的当前已发布版本（没有 = 0）；与库里不一致回 409，免得互相盖掉对方的修改。
    #[serde(default)]
    pub base_version: Option<i32>,
    #[serde(default)]
    pub note: Option<String>,
}

impl RulesInput {
    /// 备注：去空白，空串当没有，最长 255 字符（列宽）。
    pub(super) fn note(&self) -> Result<Option<&str>, AppError> {
        note_of(self.note.as_deref())
    }
}

/// 备注：去空白，空串当没有，最长 255 字符（列宽）。
pub(super) fn note_of(note: Option<&str>) -> Result<Option<&str>, AppError> {
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > 255) {
        return Err(AppError::bad_request().with_param("note"));
    }
    Ok(note)
}

/// 通过校验、可以发布的规则集。
pub(super) struct PreparedRules {
    /// 规范化形态（默认值省略）；落库的就是它。
    pub rules: Value,
    pub count: usize,
    /// 相对当前已发布版本，新增了需要 `compat.sensitive` 的操作。
    pub sensitive_added: bool,
}

pub(super) fn invalid_rules(issues: &[Issue]) -> AppError {
    let param = issues
        .first()
        .map_or_else(String::new, |issue| match &issue.rule {
            Some(rule) => format!("{rule}:{}", issue.code),
            None => issue.code.to_owned(),
        });
    AppError::new(StatusCode::BAD_REQUEST, INVALID_RULES).with_param(param)
}

/// 厂商：内置目录里有的（裸 id 当内置），或已存在的适配档案（`custom:<slug>`）；落库带命名空间。
pub(super) async fn normalize_vendor(state: &AppState, raw: &str) -> Result<String, AppError> {
    let raw = raw.trim().to_ascii_lowercase();
    if let Some(slug) = compat::profile_slug(&raw) {
        return compat::profile_by_slug(&state.pg, slug)
            .await?
            .map(|profile| format!("{}{}", compat::PROFILE_PREFIX, profile.slug))
            .ok_or_else(|| AppError::bad_request().with_param("vendor"));
    }
    vendors::get(&raw)
        .map(okapi_compat::vendors::Vendor::namespaced)
        .ok_or_else(|| AppError::bad_request().with_param("vendor"))
}

/// 校验规则集并算出「相对 `previous` 新增了哪些需要 `compat.sensitive` 的操作」。
pub(super) fn check_rules(
    actor: &AuthedKey,
    rules: &Value,
    previous: Option<&Value>,
) -> Result<PreparedRules, AppError> {
    let valid = okapi_compat::validate(rules).map_err(|issues| invalid_rules(&issues))?;
    let previous: HashSet<String> = previous
        .and_then(|old| okapi_compat::validate(old).ok())
        .map(|old| old.sensitive().iter().cloned().collect())
        .unwrap_or_default();
    let sensitive_added = valid.sensitive().iter().any(|op| !previous.contains(op));
    if sensitive_added && !actor.has_permission(permissions::COMPAT_SENSITIVE) {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param(permissions::COMPAT_SENSITIVE),
        );
    }
    Ok(PreparedRules {
        rules: valid.to_value(),
        count: valid.len(),
        sensitive_added,
    })
}

/// 校验规则集；新增敏感写入但没有 `compat.sensitive` 回 403。
/// `scope` 是 `None` 表示渠道还不存在（创建时），此时没有「已发布版本」可比。
pub(super) async fn prepare_rules(
    state: &AppState,
    actor: &AuthedKey,
    scope: Option<Scope>,
    input: &RulesInput,
) -> Result<PreparedRules, AppError> {
    // 备注也在写任何东西之前查：不能让它在发布时才失败、留下改了一半的渠道
    input.note()?;
    let previous = match scope {
        Some(scope) => compat::published(&state.pg, scope)
            .await?
            .map(|row| row.rules),
        None => None,
    };
    check_rules(actor, &input.rules, previous.as_ref())
}

/// 版本检查提前到写任何东西之前：同一个请求里还带着别的改动（厂商、渠道字段）时，
/// 冲突不能留下改了一半的渠道。发布时事务里还会在行锁下再查一次。
pub(super) async fn ensure_base_version(
    state: &AppState,
    scope: Scope,
    base: Option<i32>,
) -> Result<(), AppError> {
    let Some(base) = base else {
        return Ok(());
    };
    let current = compat::published(&state.pg, scope)
        .await?
        .map_or(0, |row| row.version);
    if base == current {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::CONFLICT,
            "rules_version_conflict",
        ))
    }
}

/// 新版本 + 发布（一个事务），随后让候选缓存失效。返回新版本号。
pub(super) async fn publish_rules(
    state: &AppState,
    actor: &AuthedKey,
    scope: Scope,
    prepared: &PreparedRules,
    input: &RulesInput,
) -> Result<i32, AppError> {
    let note = input.note()?;
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let version = compat::publish(
        &mut tx,
        scope,
        &prepared.rules,
        note,
        Some(actor.actor_user_id()),
        input.base_version,
    )
    .await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    state.invalidate_routing_caches();
    Ok(version)
}

async fn ensure_channel_exists(state: &AppState, id: i64) -> Result<(), AppError> {
    okapi_store::admin::channel_owner(&state.pg, id)
        .await?
        .map(|_| ())
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))
}

/// 渠道当前的厂商（列不存在渠道时 `None`）。
async fn vendor_and_base(
    state: &AppState,
    id: i64,
) -> Result<Option<(Option<String>, Option<String>)>, AppError> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT vendor, api_base FROM channels WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(row)
}

pub(super) fn row_json(row: &compat::RuleSetRow) -> Value {
    json!({
        "version": row.version,
        "status": row.status,
        "rules": row.rules,
        "note": row.note,
        "created_by": row.created_by,
        "created_at": row.created_at,
        "published_by": row.published_by,
        "published_at": row.published_at,
        "base_version": row.base_version,
        "source": row.source,
        "rationale": row.rationale,
    })
}

/// 有效规则的展示形态：按执行顺序，附来源层；被盖住的单列。
pub(super) fn effective_json(effective: &okapi_compat::EffectiveRules) -> Value {
    let rules: Vec<Value> = effective
        .rules()
        .map(|entry| json!({"layer": entry.layer, "rule": entry.rule}))
        .collect();
    json!({
        "layers": effective.layers(),
        "rules": rules,
        "shadowed": effective.shadowed(),
    })
}

/// 渠道层的输入（拥有型，装配有效规则用）。
pub(super) struct OwnedChannelLayer {
    pub channel_id: i64,
    pub version: i32,
    pub rules: Value,
}

/// 装配一个渠道（或「假想中的渠道」）的有效规则：厂商 / 档案 ⊕ 渠道层 ⊕ 全局护栏（已发布的）。
pub(super) async fn effective_for(
    state: &AppState,
    vendor: Option<&str>,
    channel: Option<OwnedChannelLayer>,
) -> Result<okapi_compat::EffectiveRules, AppError> {
    let profile = match vendor.and_then(compat::profile_slug) {
        Some(slug) => match compat::profile_by_slug(&state.pg, slug).await? {
            Some(row) => {
                let published = compat::published(&state.pg, Scope::Profile(row.id)).await?;
                Some((row, published))
            }
            None => None,
        },
        None => None,
    };
    let guard_row = compat::published(&state.pg, Scope::Global).await?;
    let guard_layer = guard_row
        .as_ref()
        .and_then(|row| compat::guard_layer(row.version, row.schema_version, &row.rules));
    Ok(compat::assemble(&Layers {
        vendor,
        profile: profile
            .as_ref()
            .map(|(row, published)| compat::ProfileLayer {
                slug: &row.slug,
                extends: row.extends.as_deref(),
                published: published.as_ref().map(|set| compat::PublishedSet {
                    version: set.version,
                    schema_version: set.schema_version,
                    rules: &set.rules,
                }),
            }),
        channel: channel.as_ref().map(|layer| ChannelLayer {
            channel_id: layer.channel_id,
            version: layer.version,
            schema_version: i32::try_from(okapi_compat::SCHEMA_VERSION).unwrap_or(1),
            rules: &layer.rules,
        }),
        guard: guard_layer.as_ref(),
    }))
}

/// GET /admin/channels/{id}/compat
pub async fn get_channel_compat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    Ok(Json(channel_view(&state, &actor, scope, id).await?))
}

/// 渠道的适配规则全景：厂商 / 档案、渠道层与草稿、护栏、合并后的有效规则（REST 与 MCP 共用）。
pub(super) async fn channel_view(
    state: &AppState,
    actor: &AuthedKey,
    scope: PermScope,
    id: i64,
) -> Result<Value, AppError> {
    ensure_channel_owner(state, id, actor, scope).await?;
    let (vendor, api_base) = vendor_and_base(state, id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    let channel = compat::published(&state.pg, Scope::Channel(id)).await?;
    let draft = compat::draft(&state.pg, Scope::Channel(id)).await?;
    let guard_row = compat::published(&state.pg, Scope::Global).await?;
    let effective = effective_for(
        state,
        vendor.as_deref(),
        channel.as_ref().map(|row| OwnedChannelLayer {
            channel_id: id,
            version: row.version,
            rules: row.rules.clone(),
        }),
    )
    .await?;
    // 没选厂商时按地址主机给个建议：只建议，不会静默应用
    let suggestion = if vendor.is_none() {
        suggest(state, api_base.as_deref()).await?
    } else {
        None
    };
    let profile = match vendor.as_deref().and_then(compat::profile_slug) {
        Some(slug) => compat::profile_by_slug(&state.pg, slug)
            .await?
            .map(|p| json!({"id": p.id, "slug": p.slug, "name": p.name, "extends": p.extends})),
        None => None,
    };
    Ok(json!({
        "vendor": vendor,
        "profile": profile,
        "suggested_vendor": suggestion,
        "channel": channel.as_ref().map(row_json),
        "draft": draft.as_ref().map(row_json),
        "guard": guard_row.as_ref().map(row_json),
        "effective": effective_json(&effective),
    }))
}

/// 按地址主机给厂商 / 档案的建议（只建议，不静默应用）：内置厂商优先，其次档案的主机清单。
async fn suggest(state: &AppState, api_base: Option<&str>) -> Result<Option<Value>, AppError> {
    let Some(host) = compat::host_of(api_base) else {
        return Ok(None);
    };
    if let Some(vendor) = vendors::suggest_for_host(&host) {
        return Ok(Some(
            json!({"id": vendor.namespaced(), "display_name": vendor.display_name, "kind": "builtin"}),
        ));
    }
    let profiles = compat::list_profiles(&state.pg).await?;
    Ok(profiles
        .iter()
        .find(|p| {
            p.hosts
                .iter()
                .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        })
        .map(|p| json!({"id": format!("{}{}", compat::PROFILE_PREFIX, p.slug), "display_name": p.name, "kind": "profile"})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutCompatReq {
    /// 缺省 = 不改；`null` = 清掉厂商；字符串 = 选定内置厂商或适配档案（`custom:<slug>`）。
    #[serde(default, deserialize_with = "super::double_option")]
    pub vendor: Option<Option<String>>,
    /// 缺省 = 不改渠道层规则。
    #[serde(default)]
    pub rules: Option<Value>,
    #[serde(default)]
    pub base_version: Option<i32>,
    #[serde(default)]
    pub note: Option<String>,
}

/// PUT /admin/channels/{id}/compat
pub async fn put_channel_compat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(req): ExtractJson<PutCompatReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    ensure_channel_exists(&state, id).await?;
    let vendor = match &req.vendor {
        Some(Some(raw)) => Some(Some(normalize_vendor(&state, raw).await?)),
        Some(None) => Some(None),
        None => None,
    };
    let input = req.rules.as_ref().map(|rules| RulesInput {
        rules: rules.clone(),
        base_version: req.base_version,
        note: req.note.clone(),
    });
    let prepared = match &input {
        Some(input) => {
            ensure_base_version(&state, Scope::Channel(id), input.base_version).await?;
            Some(prepare_rules(&state, &actor, Some(Scope::Channel(id)), input).await?)
        }
        None => None,
    };
    if let Some(vendor) = &vendor {
        let patch = okapi_store::admin::ChannelPatch {
            vendor: Some(vendor.as_deref()),
            ..okapi_store::admin::ChannelPatch::default()
        };
        okapi_store::admin::patch_channel(&state.pg, id, patch).await?;
        state.invalidate_routing_caches();
    }
    let version = match (&prepared, &input) {
        (Some(prepared), Some(input)) => {
            Some(publish_rules(&state, &actor, Scope::Channel(id), prepared, input).await?)
        }
        _ => None,
    };
    audit(
        &state,
        &actor,
        "channel.compat",
        &id.to_string(),
        json!({
            "vendor": vendor, "version": version,
            "rules": prepared.as_ref().map(|p| p.count),
            "sensitive_added": prepared.as_ref().is_some_and(|p| p.sensitive_added),
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "version": version })))
}

/// GET /admin/compat/guard
pub async fn get_guard(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let row = compat::published(&state.pg, Scope::Global).await?;
    let draft = compat::draft(&state.pg, Scope::Global).await?;
    Ok(Json(json!({
        "guard": row.as_ref().map(row_json),
        "draft": draft.as_ref().map(row_json),
    })))
}

/// PUT /admin/compat/guard：全局护栏，影响所有渠道。
pub async fn put_guard(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(input): ExtractJson<RulesInput>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::COMPAT_GUARD_WRITE).await?;
    let prepared = prepare_rules(&state, &actor, Some(Scope::Global), &input).await?;
    let version = publish_rules(&state, &actor, Scope::Global, &prepared, &input).await?;
    audit(
        &state,
        &actor,
        "compat.guard",
        "global",
        json!({
            "version": version, "rules": prepared.count,
            "sensitive_added": prepared.sensitive_added,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "version": version })))
}

#[derive(Debug, Deserialize)]
pub struct VendorsQuery {
    /// 渠道地址的主机名：命中就在 `suggestion` 里给出建议。
    pub host: Option<String>,
}

/// GET /admin/compat/vendors：内置厂商目录与适配档案。
pub async fn list_vendors(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<VendorsQuery>,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let data: Vec<Value> = vendors::all()
        .iter()
        .map(|v| {
            json!({
                "id": v.namespaced(), "display_name": v.display_name, "group": v.group,
                "hosts": v.hosts, "endpoints": v.endpoints, "probes": v.probes,
                "rules": v.rules().len(),
            })
        })
        .collect();
    let profiles: Vec<Value> = compat::list_profiles(&state.pg)
        .await?
        .iter()
        .map(|p| {
            json!({
                "id": format!("{}{}", compat::PROFILE_PREFIX, p.slug), "slug": p.slug,
                "display_name": p.name, "description": p.description, "extends": p.extends,
                "hosts": p.hosts,
            })
        })
        .collect();
    let suggestion = match query.host.as_deref() {
        Some(host) => suggest(&state, Some(&format!("https://{host}"))).await?,
        None => None,
    };
    Ok(Json(
        json!({ "data": data, "profiles": profiles, "suggestion": suggestion }),
    ))
}

/// GET /admin/compat/catalog：规则的自描述目录。
pub async fn catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    Ok(Json(okapi_compat::catalog::catalog()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DryRunError {
    /// 上游返回的 HTTP 状态。
    pub status: u16,
    /// 错误体：JSON 或原文字符串。
    #[serde(default)]
    pub body: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DryRunReq {
    /// 用这个渠道已发布的厂商 / 渠道层 / 护栏（再按下面的字段覆盖）。
    #[serde(default)]
    pub channel_id: Option<i64>,
    /// 覆盖厂商（不带 channel_id 时，用来试「选了某厂商 / 档案会怎样」）。
    #[serde(default)]
    pub vendor: Option<String>,
    /// 待试的渠道层草稿，取代渠道现有的渠道层。
    #[serde(default)]
    pub rules: Option<Value>,
    /// 试哪个阶段，缺省 `request`。
    #[serde(default)]
    pub phase: Option<Phase>,
    pub context: ContextInput,
    /// `request` 阶段的样本（发往上游的那份 JSON）；`error` 阶段可选：给了就演示 `retry_same` 改完的请求体。
    #[serde(default)]
    pub request: Option<Value>,
    /// `response` 阶段的样本（OpenAI chat 响应，或流式的一帧）。
    #[serde(default)]
    pub response: Option<Value>,
    /// `error` 阶段的样本。
    #[serde(default)]
    pub error: Option<DryRunError>,
}

fn require<T>(value: Option<T>, param: &'static str) -> Result<T, AppError> {
    value.ok_or_else(|| AppError::bad_request().with_param(param))
}

/// POST /admin/compat/dry-run：不落库、不发上游；校验草稿并在样本上执行，给出前后差异与命中轨迹。
pub async fn dry_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<DryRunReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    Ok(Json(run_dry(&state, &actor, scope, req).await?))
}

/// 试跑的实现（REST 与 MCP `compat_dry_run` 共用）。
#[allow(clippy::too_many_lines)]
pub(super) async fn run_dry(
    state: &AppState,
    actor: &AuthedKey,
    scope: PermScope,
    req: DryRunReq,
) -> Result<Value, AppError> {
    let phase = req.phase.unwrap_or(Phase::Request);
    let mut vendor = match req.vendor.as_deref() {
        Some(raw) => Some(normalize_vendor(state, raw).await?),
        None => None,
    };
    let mut channel_rules = None;
    let mut channel_version = 0;
    let mut channel_capabilities = Value::Null;
    let mut context = req.context.clone();
    if let Some(id) = req.channel_id {
        ensure_channel_owner(state, id, actor, scope).await?;
        let (channel_vendor, api_base) = vendor_and_base(state, id)
            .await?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
        vendor = vendor.or(channel_vendor);
        if context.host.is_none() {
            context.host = compat::host_of(api_base.as_deref());
        }
        if let Some(row) = compat::published(&state.pg, Scope::Channel(id)).await? {
            channel_version = row.version;
            channel_rules = Some(row.rules);
        }
        channel_capabilities =
            sqlx::query_scalar("SELECT capabilities FROM channels WHERE id = $1")
                .bind(id)
                .fetch_one(&state.pg)
                .await
                .map_err(okapi_store::StoreError::from)?;
    }
    if context.vendor.is_none() {
        context.vendor.clone_from(&vendor);
    }
    if let Some(draft) = &req.rules {
        match okapi_compat::validate(draft) {
            Ok(valid) => channel_rules = Some(valid.to_value()),
            Err(issues) => return Ok(json!({ "valid": false, "issues": issues })),
        }
    }
    let effective = effective_for(
        state,
        vendor.as_deref(),
        channel_rules.map(|rules| OwnedChannelLayer {
            channel_id: req.channel_id.unwrap_or(0),
            version: channel_version,
            rules,
        }),
    )
    .await?;
    let ctx = context.as_context();
    let layers = effective.layers();
    let result = match phase {
        Phase::Request => {
            let before = require(req.request, "request")?;
            let mut after = before.clone();
            let report = effective
                .apply_request_value(&ctx, &mut after)
                .map_err(|failure| {
                    AppError::new(StatusCode::UNPROCESSABLE_ENTITY, INVALID_RULES)
                        .with_param(failure.to_string())
                })?;
            json!({
                "changed": after != before, "after": after,
                "fired": report.fired, "skipped": report.skipped,
            })
        }
        Phase::Response => {
            let before = require(req.response, "response")?;
            let mut after = before.clone();
            let report = effective.apply_response_value(&ctx, &mut after);
            json!({
                "changed": after != before, "after": after,
                "fired": report.fired, "skipped": report.skipped,
            })
        }
        Phase::Error => {
            let error = require(req.error, "error")?;
            let body = match &error.body {
                Value::String(text) => text.clone().into_bytes(),
                other => other.to_string().into_bytes(),
            };
            let sent = req.request.as_ref().map(Value::to_string);
            let mut chosen = None;
            let reactions: Vec<Value> = effective
                .reactions(&ctx, error.status, &body)
                .iter()
                .enumerate()
                .map(|(index, reaction)| {
                    let (action, viable, detail) = match reaction.action {
                        Action::RetrySame { mutation, max } => {
                            let resend = sent
                                .as_deref()
                                .and_then(|sent| mutation.apply(sent.as_bytes()))
                                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                            (
                                "retry_same",
                                resend.is_some() || sent.is_none(),
                                json!({"max": max, "resend": resend}),
                            )
                        }
                        Action::Failover { cooldown_secs } => {
                            ("failover", true, json!({"cooldown_secs": cooldown_secs}))
                        }
                        Action::Fail { status, code } => {
                            ("fail", true, json!({"status": status, "code": code}))
                        }
                    };
                    if viable && chosen.is_none() {
                        chosen = Some(index);
                    }
                    json!({
                        "rule": reaction.rule, "layer": reaction.layer,
                        "action": action, "viable": viable, "detail": detail,
                    })
                })
                .collect();
            json!({ "matched": !reactions.is_empty(), "chosen": chosen, "reactions": reactions })
        }
        Phase::Capability => {
            let facts = effective.capabilities(&ctx);
            let merged = compat::merge_capabilities(channel_capabilities.clone(), &facts);
            json!({ "facts": facts, "channel_capabilities": channel_capabilities, "merged": merged })
        }
        Phase::RequestPre | Phase::Wire => {
            return Err(AppError::bad_request().with_param("phase"));
        }
    };
    let mut out = json!({ "valid": true, "phase": phase, "layers": layers });
    if let (Value::Object(out), Value::Object(result)) = (&mut out, result) {
        out.extend(result);
    }
    Ok(out)
}
