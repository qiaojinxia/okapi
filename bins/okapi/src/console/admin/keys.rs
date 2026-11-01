//! 管理面改 / 删任意用户的 API key。
use super::{audit, ensure_group_usable, guard};
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use serde::Deserialize;
use serde_json::{Value, json};

// ---- API key 管控（管理面：限额与分组覆盖）----

#[derive(Deserialize)]
pub struct AdminPatchKeyReq {
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub quota_micro: Option<Option<i64>>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub expires_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub model_allowlist: Option<Option<Vec<String>>>,
    /// key 级定价组覆盖；null = 回落用户组。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub group_override: Option<Option<String>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub rpm_limit: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub tpm_limit: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub rpd_limit: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub daily_token_limit: Option<Option<i64>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub max_concurrency: Option<Option<i32>>,
    /// IP 白名单（地址 / CIDR 数组；null / 空 = 解除）。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub ip_allowlist: Option<Option<Vec<String>>>,
}

/// 限额一律取正数：0 或负数会把 key 限死成不可用，属于误配而非合法配置。
// 入参来自 double_option 的三态补丁值，豁免理由同 console::double_option
#[allow(clippy::option_option)]
pub(super) fn ensure_positive(field: &str, value: Option<Option<i64>>) -> Result<(), AppError> {
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
    ExtractJson(req): ExtractJson<AdminPatchKeyReq>,
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
    super::super::portal::validate_key_limits(req.quota_micro.flatten(), None)?;
    ensure_positive(
        "max_concurrency",
        req.max_concurrency.map(|v| v.map(i64::from)),
    )?;
    if let Some(Some(group)) = &req.group_override {
        ensure_group_usable(&state, group, "group_override").await?;
    }

    let patch = okapi_store::admin::ApiKeyPatch {
        quota_micro: req.quota_micro,
        name: req
            .name
            .as_deref()
            .map(|name| {
                super::super::identifiers::ensure_optional_text("name", name, 128)
                    .map(str::to_owned)
            })
            .transpose()?,
        status: req.status,
        expires_at: req.expires_at,
        model_allowlist: req
            .model_allowlist
            .clone()
            .map(super::super::portal::normalize_allowlist),
        group_override: req.group_override.clone(),
        rpm_limit: req.rpm_limit,
        tpm_limit: req.tpm_limit,
        rpd_limit: req.rpd_limit,
        daily_token_limit: req.daily_token_limit,
        max_concurrency: req.max_concurrency,
        ip_allowlist: match req.ip_allowlist.clone() {
            Some(list) => Some(super::super::portal::normalize_ip_allowlist(list)?),
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
