//! 渠道多 Key 生命周期与映射/能力校验：新增 key、删除 key、更新单把 key 调度与流控护栏。
use super::channels::normalize_models;
use super::{audit, ensure_channel_owner, guard_scoped};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_kernel::extract::Path;
use serde::Deserialize;
use serde_json::{Value, json};

/// 校验并规整 `model_mapping`：对象，所有键值均须为非空非控制字符字符串（<=128 字节），去首尾空白。
pub(super) fn normalize_model_mapping(value: &Value) -> Result<Value, AppError> {
    let Some(map) = value.as_object() else {
        return Err(AppError::bad_request().with_param("model_mapping"));
    };
    if map.len() > 512 {
        return Err(AppError::bad_request().with_param("model_mapping"));
    }
    let mut out = serde_json::Map::with_capacity(map.len());
    for (k, v) in map {
        let key = k.trim();
        let Some(target) = v.as_str().map(str::trim) else {
            return Err(AppError::bad_request().with_param("model_mapping"));
        };
        if key.is_empty()
            || target.is_empty()
            || key.len() > 128
            || target.len() > 128
            || key.chars().any(char::is_control)
            || target.chars().any(char::is_control)
        {
            return Err(AppError::bad_request().with_param("model_mapping"));
        }
        out.insert(key.to_owned(), Value::String(target.to_owned()));
    }
    Ok(Value::Object(out))
}

/// 校验 `capabilities`：对象，键非空 <=64 字节且无控制字符，值须为布尔（如 `tools`/`vision`）。
pub(super) fn validate_capabilities(value: &Value) -> Result<(), AppError> {
    let Some(map) = value.as_object() else {
        return Err(AppError::bad_request().with_param("capabilities"));
    };
    if map.len() > 64 {
        return Err(AppError::bad_request().with_param("capabilities"));
    }
    for (k, v) in map {
        if k.trim().is_empty() || k.len() > 64 || k.chars().any(char::is_control) || !v.is_boolean()
        {
            return Err(AppError::bad_request().with_param("capabilities"));
        }
    }
    Ok(())
}

fn normalize_subset(raw: Option<Vec<String>>) -> Result<Option<Value>, AppError> {
    let Some(list) = raw else {
        return Ok(None);
    };
    if list.is_empty() {
        return Ok(None);
    }
    let models = normalize_models(&list)?;
    Ok(Some(json!(models)))
}

#[derive(Deserialize)]
pub struct AddChannelKeyReq {
    pub credential: String,
    #[serde(default = "default_weight")]
    pub weight: i32,
    #[serde(default)]
    pub max_concurrency: Option<i32>,
    #[serde(default)]
    pub model_subset: Option<Vec<String>>,
    #[serde(default)]
    pub rpm_limit: Option<i32>,
    #[serde(default)]
    pub daily_spend_cap_micro: Option<i64>,
}

const fn default_weight() -> i32 {
    1
}

/// POST /admin/channels/{id}/keys：向已有渠道追加一把上游凭证 key。
pub async fn add_channel_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(req): ExtractJson<AddChannelKeyReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    if req.credential.trim().is_empty() {
        return Err(AppError::bad_request().with_param("credential"));
    }
    if req.weight < 0 {
        return Err(AppError::bad_request().with_param("weight"));
    }
    if req.max_concurrency.is_some_and(|c| c <= 0) {
        return Err(AppError::bad_request().with_param("max_concurrency"));
    }
    if req.rpm_limit.is_some_and(|r| r <= 0) {
        return Err(AppError::bad_request().with_param("rpm_limit"));
    }
    super::super::portal::validate_budget_cap("daily_spend_cap_micro", req.daily_spend_cap_micro)?;
    let subset = normalize_subset(req.model_subset)?;
    okapi_engine::credentials::validate_credential(&state.pg, &req.credential).await?;
    let provider: String =
        sqlx::query_scalar("SELECT provider FROM channels WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    let credential =
        super::super::channel_credentials::normalize(&provider, req.credential.trim())?;
    let Some(key_id) = okapi_store::admin::add_channel_key(
        &state.pg,
        id,
        okapi_store::admin::NewChannelKey {
            credential: &credential,
            weight: req.weight,
            max_concurrency: req.max_concurrency,
            model_subset: subset.as_ref(),
            rpm_limit: req.rpm_limit,
            daily_spend_cap_micro: req.daily_spend_cap_micro,
        },
        state.master_key.as_deref(),
    )
    .await?
    else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.add_key",
        &format!("{id}/{key_id}"),
        json!({
            "weight": req.weight,
            "max_concurrency": req.max_concurrency,
            "model_subset": subset,
            "rpm_limit": req.rpm_limit,
            "daily_spend_cap_micro": req.daily_spend_cap_micro,
        }),
    )
    .await;
    Ok(Json(json!({ "channel_key_id": key_id })))
}

/// DELETE /admin/channels/{id}/keys/{key_id}：移除多 key 渠道的一把 key（禁止删空最后一把）。
pub async fn delete_channel_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, key_id)): Path<(i64, i64)>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    match okapi_store::admin::delete_channel_key(&state.pg, id, key_id).await? {
        okapi_store::admin::DeleteChannelKeyOutcome::Deleted => {}
        okapi_store::admin::DeleteChannelKeyOutcome::NotFound => {
            return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
        }
        okapi_store::admin::DeleteChannelKeyOutcome::LastKey => {
            return Err(AppError::new(StatusCode::CONFLICT, "channel_last_key"));
        }
    }
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.delete_key",
        &format!("{id}/{key_id}"),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
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
    /// key 级模型子集（null / 空数组 = 继承渠道全部模型）。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub model_subset: Option<Option<Vec<String>>>,
    /// key 级 RPM 上限（null = 不限）。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub rpm_limit: Option<Option<i32>>,
    /// key 级每日消费上限 micro-USD（null = 不限）。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub daily_spend_cap_micro: Option<Option<i64>>,
}

/// PATCH /admin/channels/{id}/keys/{key_id}：调单把 key 的权重、并发、模型子集、RPM、日消费上限与状态。
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
    if req.rpm_limit.flatten().is_some_and(|r| r <= 0) {
        return Err(AppError::bad_request().with_param("rpm_limit"));
    }
    super::super::portal::validate_budget_cap(
        "daily_spend_cap_micro",
        req.daily_spend_cap_micro.flatten(),
    )?;
    if req.status.is_some_and(|s| !matches!(s, 1 | 2)) {
        return Err(AppError::bad_request().with_param("status"));
    }
    let model_subset = match req.model_subset {
        Some(list) => Some(normalize_subset(list)?),
        None => None,
    };
    let patch = okapi_store::admin::ChannelKeyPatch {
        weight: req.weight,
        max_concurrency: req.max_concurrency,
        status: req.status,
        model_subset: model_subset.clone(),
        rpm_limit: req.rpm_limit,
        daily_spend_cap_micro: req.daily_spend_cap_micro,
    };
    let hit = okapi_store::admin::patch_channel_key(&state.pg, id, key_id, &patch).await?;
    if !hit {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    if req.status == Some(1) {
        let models: Option<Value> = sqlx::query_scalar("SELECT models FROM channels WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pg)
            .await
            .ok()
            .flatten();
        let list: Vec<String> = models
            .and_then(|v| v.as_array().cloned())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        state.sched.clear_channel_key_cooldowns(key_id, &list).await;
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
            "model_subset": model_subset,
            "rpm_limit": req.rpm_limit,
            "daily_spend_cap_micro": req.daily_spend_cap_micro,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}
