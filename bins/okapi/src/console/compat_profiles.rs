//! 适配档案与「这个请求命中了哪些规则」（设计见 docs/compat-rules.md §17.5、§12.2）。
//!
//! - `GET / POST  /admin/compat/profiles`：档案列表（带线上版本、草稿、被几个渠道在用）/ 新建；
//! - `GET / PATCH / DELETE /admin/compat/profiles/{id}`：详情（含有效规则预览）/ 改名称说明主机 / 删除；
//! - `GET /admin/compat/explain/{request_id}`：按请求 id 读诊断里记下的命中轨迹，并把用到的各版规则一并取出来。
//!
//! 档案的规则集（草稿、版本、发布）走 [`super::compat_sets`] 的 `profile` 范围。
//! 档案一改会影响所有选了它的渠道，所以写入要 `compat.profile.write`，发布还要 `compat.publish`。

use super::admin::{audit, guard, guard_scoped};
use super::compat::{OwnedChannelLayer, effective_for, effective_json, row_json};
use crate::gateway::error::AppError;
use crate::gateway::extract::{Json as ExtractJson, Path};
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_compat::vendors;
use okapi_store::compat::{self, ProfileInput, ProfileRow, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

fn profile_json(profile: &ProfileRow) -> Value {
    json!({
        "id": profile.id, "slug": profile.slug, "name": profile.name,
        "description": profile.description, "extends": profile.extends, "hosts": profile.hosts,
        "vendor": format!("{}{}", compat::PROFILE_PREFIX, profile.slug),
        "created_by": profile.created_by, "created_at": profile.created_at, "updated_at": profile.updated_at,
    })
}

fn not_found() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
}

/// slug：与表的 CHECK 同一形状（小写字母 / 数字开头，之后可带 `. _ -`），至多 64 字符。
fn ensure_slug(slug: &str) -> Result<(), AppError> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(AppError::bad_request().with_param("slug"))
    }
}

fn ensure_name(name: &str) -> Result<&str, AppError> {
    super::identifiers::ensure_display_name("name", name, 128)
}

fn ensure_description(text: Option<&str>) -> Result<Option<&str>, AppError> {
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    if text.is_some_and(|t| t.chars().count() > 512 || t.chars().any(char::is_control)) {
        return Err(AppError::bad_request().with_param("description"));
    }
    Ok(text)
}

/// 主机建议清单：小写主机名，至多 16 个，不带协议 / 路径 / 通配。
fn ensure_hosts(hosts: &[String]) -> Result<Vec<String>, AppError> {
    let mut out: Vec<String> = Vec::with_capacity(hosts.len());
    for raw in hosts {
        let host = raw.trim().to_ascii_lowercase();
        let ok = !host.is_empty()
            && host.len() <= 253
            && host
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'))
            && !host.starts_with(['.', '-'])
            && !host.ends_with(['.', '-']);
        if !ok {
            return Err(AppError::bad_request().with_param("hosts"));
        }
        if !out.contains(&host) {
            out.push(host);
        }
    }
    if out.len() > 16 {
        return Err(AppError::bad_request().with_param("hosts"));
    }
    Ok(out)
}

/// 叠加的内置厂商：必须在目录里。
fn ensure_extends(raw: &str) -> Result<String, AppError> {
    vendors::get(raw.trim())
        .map(okapi_compat::vendors::Vendor::namespaced)
        .ok_or_else(|| AppError::bad_request().with_param("extends"))
}

/// GET /admin/compat/profiles
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let profiles = compat::list_profiles(&state.pg).await?;
    let usage = compat::profile_usage(&state.pg).await?;
    let mut data = Vec::with_capacity(profiles.len());
    for profile in &profiles {
        let published = compat::published(&state.pg, Scope::Profile(profile.id)).await?;
        let draft = compat::draft(&state.pg, Scope::Profile(profile.id)).await?;
        let mut item = profile_json(profile);
        item["version"] = json!(published.as_ref().map(|row| row.version));
        item["rules"] = json!(
            published
                .as_ref()
                .and_then(|row| row.rules.as_array())
                .map_or(0, Vec::len)
        );
        item["has_draft"] = json!(draft.is_some());
        item["channels"] = json!(usage.get(&profile.slug).copied().unwrap_or(0));
        data.push(item);
    }
    Ok(Json(json!({ "data": data })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProfileReq {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 叠在哪个内置厂商之上（`deepseek` 或 `builtin:deepseek`）。
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// POST /admin/compat/profiles
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<CreateProfileReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::COMPAT_PROFILE_WRITE).await?;
    let slug = req.slug.trim().to_ascii_lowercase();
    ensure_slug(&slug)?;
    let name = ensure_name(&req.name)?;
    let description = ensure_description(req.description.as_deref())?;
    let extends = req.extends.as_deref().map(ensure_extends).transpose()?;
    let hosts = ensure_hosts(&req.hosts)?;
    let profile = compat::create_profile(
        &state.pg,
        ProfileInput {
            slug: &slug,
            name,
            description,
            extends: extends.as_deref(),
            hosts: &hosts,
        },
        Some(actor.actor_user_id()),
    )
    .await?;
    audit(
        &state,
        &actor,
        "compat.profile.create",
        &profile.id.to_string(),
        json!({"slug": profile.slug, "extends": profile.extends}),
    )
    .await;
    Ok(Json(profile_json(&profile)))
}

/// GET /admin/compat/profiles/{id}
pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let profile = compat::profile(&state.pg, id)
        .await?
        .ok_or_else(not_found)?;
    let published = compat::published(&state.pg, Scope::Profile(id)).await?;
    let draft = compat::draft(&state.pg, Scope::Profile(id)).await?;
    let usage = compat::profile_usage(&state.pg).await?;
    // 选了这个档案的渠道会拿到的有效规则（不含渠道层）：厂商层（extends）+ 档案层 + 全局护栏
    let vendor = format!("{}{}", compat::PROFILE_PREFIX, profile.slug);
    let effective = effective_for(&state, Some(&vendor), None::<OwnedChannelLayer>).await?;
    let mut item = profile_json(&profile);
    item["published"] = json!(published.as_ref().map(row_json));
    item["draft"] = json!(draft.as_ref().map(row_json));
    item["channels"] = json!(usage.get(&profile.slug).copied().unwrap_or(0));
    item["effective"] = effective_json(&effective);
    Ok(Json(item))
}

// description / extends 要区分「没传（不动）」与「传了 null（清空）」，所以是 Option<Option<_>>
#[allow(clippy::option_option)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchProfileReq {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub extends: Option<Option<String>>,
    #[serde(default)]
    pub hosts: Option<Vec<String>>,
}

/// PATCH /admin/compat/profiles/{id}
pub async fn patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(req): ExtractJson<PatchProfileReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::COMPAT_PROFILE_WRITE).await?;
    let name = req.name.as_deref().map(ensure_name).transpose()?;
    let description = match &req.description {
        Some(Some(text)) => Some(ensure_description(Some(text))?),
        Some(None) => Some(None),
        None => None,
    };
    let extends = match &req.extends {
        Some(Some(raw)) => Some(Some(ensure_extends(raw)?)),
        Some(None) => Some(None),
        None => None,
    };
    let hosts = req.hosts.as_deref().map(ensure_hosts).transpose()?;
    let profile = compat::update_profile(
        &state.pg,
        id,
        name,
        description,
        extends.as_ref().map(Option::as_deref),
        hosts.as_deref(),
    )
    .await?
    .ok_or_else(not_found)?;
    // 档案叠加的厂商或主机变了，所有选了它的渠道的有效规则跟着变
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "compat.profile.update",
        &id.to_string(),
        json!({"slug": profile.slug}),
    )
    .await;
    Ok(Json(profile_json(&profile)))
}

/// DELETE /admin/compat/profiles/{id}：还有渠道在用返回 409。
pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::COMPAT_PROFILE_WRITE).await?;
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let deleted = compat::delete_profile(&mut tx, id).await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    if !deleted {
        return Err(not_found());
    }
    audit(
        &state,
        &actor,
        "compat.profile.delete",
        &id.to_string(),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// 诊断里的版本标签 → 当时的规则内容。取不到（版本已被清理、厂商已下线）给 `null`。
async fn label_rules(state: &AppState, label: &str) -> Result<Value, AppError> {
    let (name, version) = match label.rsplit_once("@v") {
        Some((name, version)) => (name, version.parse::<i32>().ok()),
        None => (label, None),
    };
    let (layer, rules) = if let Some(id) = name
        .strip_prefix("channel#")
        .and_then(|s| s.parse::<i64>().ok())
    {
        let row = match version {
            Some(v) => compat::version(&state.pg, Scope::Channel(id), v).await?,
            None => None,
        };
        ("channel", row.map(|r| r.rules))
    } else if let Some(slug) = compat::profile_slug(name) {
        let row = match (compat::profile_by_slug(&state.pg, slug).await?, version) {
            (Some(profile), Some(v)) => {
                compat::version(&state.pg, Scope::Profile(profile.id), v).await?
            }
            _ => None,
        };
        ("profile", row.map(|r| r.rules))
    } else if name == "guard" {
        let row = match version {
            Some(v) => compat::version(&state.pg, Scope::Global, v).await?,
            None => None,
        };
        ("guard", row.map(|r| r.rules))
    } else {
        ("vendor", vendors::get(name).map(|v| v.rules().to_value()))
    };
    Ok(json!({"label": label, "layer": layer, "version": version, "rules": rules}))
}

/// GET /admin/compat/explain/{request_id}
pub async fn explain(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::BILLING_READ).await?;
    Ok(Json(explain_request(&state, request_id).await?))
}

/// 诊断里每个阶段只记「用到了哪几层的哪个版本、哪些规则命中、哪些被跳过」，不记请求或响应内容；
/// 这里把那几个版本的规则内容一并取出来，一眼就能看到「这条请求为什么被改成这样」
/// （REST 与 MCP `compat_explain` 共用）。
pub(super) async fn explain_request(state: &AppState, request_id: Uuid) -> Result<Value, AppError> {
    let row: Option<(Option<Value>, String, Option<i64>)> = sqlx::query_as(
        "SELECT usage_details -> 'diagnostics', model_name, channel_id
           FROM billing_records WHERE request_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(request_id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let (diagnostics, model, channel_id) = row.ok_or_else(not_found)?;
    let diagnostics = diagnostics.unwrap_or(Value::Null);
    let mut trace = serde_json::Map::new();
    let mut labels: Vec<String> = Vec::new();
    for key in ["compat", "compat_response", "compat_error"] {
        let Some(section) = diagnostics.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        for label in section
            .get("versions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !labels.iter().any(|known| known == label) {
                labels.push(label.to_owned());
            }
        }
        trace.insert(key.to_owned(), section.clone());
    }
    let mut layers = Vec::with_capacity(labels.len());
    for label in &labels {
        layers.push(label_rules(state, label).await?);
    }
    Ok(json!({
        "request_id": request_id, "model": model, "channel_id": channel_id,
        "trace": trace, "layers": layers,
    }))
}
