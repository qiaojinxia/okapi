//! 模型别名（`model_aliases`）与用户专属模型定价（`user_pricing`）管理面 CRUD。
use super::{audit, guard, ratio_fits};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::{Json as ExtractJson, Path};
use serde::Deserialize;
use serde_json::{Value, json};

// ---- 模型别名 CRUD（/admin/model-aliases）----

#[derive(sqlx::FromRow)]
struct AliasRow {
    id: i64,
    pattern: String,
    target_model: String,
    priority: i32,
    enabled: bool,
}

pub async fn list_model_aliases(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let rows = sqlx::query_as::<_, AliasRow>(
        "SELECT id, pattern, target_model, priority, enabled \
         FROM model_aliases ORDER BY priority DESC, pattern ASC",
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "pattern": r.pattern,
                "target_model": r.target_model,
                "priority": r.priority,
                "enabled": r.enabled,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "total": data.len() })))
}

#[derive(Deserialize)]
pub struct UpsertModelAliasReq {
    pub pattern: String,
    pub target_model: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

const fn default_true() -> bool {
    true
}

pub async fn upsert_model_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpsertModelAliasReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let pattern = req.pattern.trim();
    let target = req.target_model.trim();
    if pattern.is_empty()
        || pattern.len() > 128
        || pattern.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(AppError::bad_request().with_param("pattern"));
    }
    if target.is_empty() || target.len() > 128 || target.chars().any(char::is_control) {
        return Err(AppError::bad_request().with_param("target_model"));
    }
    if pattern == target {
        return Err(AppError::bad_request().with_param("self_alias"));
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM models WHERE model_name = $1)")
            .bind(target)
            .fetch_one(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    if !exists {
        return Err(
            AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("target_model")
        );
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO model_aliases (pattern, target_model, priority, enabled) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (pattern) DO UPDATE SET \
           target_model = EXCLUDED.target_model, priority = EXCLUDED.priority, enabled = EXCLUDED.enabled \
         RETURNING id",
    )
    .bind(pattern)
    .bind(target)
    .bind(req.priority)
    .bind(req.enabled)
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    state.invalidate_cache(okapi_engine::state::CacheInvalidation::Models);
    audit(
        &state,
        &actor,
        "pricing.upsert_alias",
        pattern,
        json!({ "id": id, "target_model": target, "priority": req.priority, "enabled": req.enabled }),
    )
    .await;
    Ok(Json(
        json!({ "id": id, "pattern": pattern, "target_model": target }),
    ))
}

pub async fn delete_model_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let pattern: Option<String> =
        sqlx::query_scalar("DELETE FROM model_aliases WHERE id = $1 RETURNING pattern")
            .bind(id)
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    let Some(pattern) = pattern else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    state.invalidate_cache(okapi_engine::state::CacheInvalidation::Models);
    audit(
        &state,
        &actor,
        "pricing.delete_alias",
        &pattern,
        json!({ "id": id }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 用户级专属模型定价 CRUD（/admin/users/{id}/pricing）----

#[derive(sqlx::FromRow)]
struct UserPricingView {
    id: i64,
    user_id: i64,
    model_name: String,
    override_kind: String,
    custom_model_ratio: Option<String>,
    custom_completion_ratio: Option<String>,
    custom_cache_ratio: Option<String>,
    custom_cache_write_ratio: Option<String>,
    custom_input_per_1m_micro: Option<i64>,
    custom_output_per_1m_micro: Option<i64>,
    reason: Option<String>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list_user_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let rows = sqlx::query_as::<_, UserPricingView>(
        "SELECT up.id, up.user_id, m.model_name, up.override_kind, \
                up.custom_model_ratio::text AS custom_model_ratio, \
                up.custom_completion_ratio::text AS custom_completion_ratio, \
                up.custom_cache_ratio::text AS custom_cache_ratio, \
                up.custom_cache_write_ratio::text AS custom_cache_write_ratio, \
                up.custom_input_per_1m_micro, up.custom_output_per_1m_micro, \
                up.reason, up.expires_at \
         FROM user_pricing up JOIN models m ON m.id = up.model_id \
         WHERE up.user_id = $1 ORDER BY m.model_name",
    )
    .bind(user_id)
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "user_id": r.user_id,
                "model_name": r.model_name,
                "override_kind": r.override_kind,
                "custom_model_ratio": r.custom_model_ratio,
                "custom_completion_ratio": r.custom_completion_ratio,
                "custom_cache_ratio": r.custom_cache_ratio,
                "custom_cache_write_ratio": r.custom_cache_write_ratio,
                "custom_input_per_1m_micro": r.custom_input_per_1m_micro,
                "custom_output_per_1m_micro": r.custom_output_per_1m_micro,
                "reason": r.reason,
                "expires_at": r.expires_at.map(|t| t.to_rfc3339()),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "total": data.len() })))
}

#[derive(Deserialize)]
pub struct UpsertUserPricingReq {
    pub model_name: String,
    /// ratio | absolute
    pub override_kind: String,
    #[serde(default)]
    pub custom_model_ratio: Option<String>,
    #[serde(default)]
    pub custom_completion_ratio: Option<String>,
    #[serde(default)]
    pub custom_cache_ratio: Option<String>,
    #[serde(default)]
    pub custom_cache_write_ratio: Option<String>,
    #[serde(default)]
    pub custom_input_per_1m_micro: Option<i64>,
    #[serde(default)]
    pub custom_output_per_1m_micro: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn validate_user_override(req: &UpsertUserPricingReq) -> Result<(), AppError> {
    for (field, opt) in [
        ("custom_model_ratio", req.custom_model_ratio.as_deref()),
        (
            "custom_completion_ratio",
            req.custom_completion_ratio.as_deref(),
        ),
        ("custom_cache_ratio", req.custom_cache_ratio.as_deref()),
        (
            "custom_cache_write_ratio",
            req.custom_cache_write_ratio.as_deref(),
        ),
    ] {
        if let Some(raw) = opt
            && !ratio_fits(raw)
        {
            return Err(AppError::bad_request().with_param(field));
        }
    }
    let max = okapi_ledger::holds::MAXIMUM_MICROS;
    match req.override_kind.as_str() {
        "ratio" if req.custom_model_ratio.is_none() => {
            Err(AppError::bad_request().with_param("custom_model_ratio"))
        }
        "absolute"
            if req
                .custom_input_per_1m_micro
                .is_none_or(|v| !(0..=max).contains(&v))
                || req
                    .custom_output_per_1m_micro
                    .is_none_or(|v| !(0..=max).contains(&v)) =>
        {
            Err(AppError::bad_request().with_param("custom_per_1m_micro"))
        }
        "ratio" | "absolute" => Ok(()),
        _ => Err(AppError::bad_request().with_param("override_kind")),
    }
}

pub async fn upsert_user_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    ExtractJson(req): ExtractJson<UpsertUserPricingReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    validate_user_override(&req)?;
    let reason = req
        .reason
        .as_deref()
        .map(|r| super::super::identifiers::ensure_optional_text("reason", r, 255))
        .transpose()?;
    let user_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)",
    )
    .bind(user_id)
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if !user_exists {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("user_id"));
    }
    let model_name = req.model_name.trim();
    let model_id: Option<i64> = sqlx::query_scalar("SELECT id FROM models WHERE model_name = $1")
        .bind(model_name)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let Some(model_id) = model_id else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("model_name"));
    };
    let id: i64 = sqlx::query_scalar(
        r"INSERT INTO user_pricing (
              user_id, model_id, override_kind, custom_model_ratio, custom_completion_ratio,
              custom_cache_ratio, custom_cache_write_ratio, custom_input_per_1m_micro,
              custom_output_per_1m_micro, reason, expires_at
          ) VALUES (
              $1, $2, $3, ($4::text)::numeric, ($5::text)::numeric,
              ($6::text)::numeric, ($7::text)::numeric, $8, $9, $10, $11
          )
          ON CONFLICT (user_id, model_id) DO UPDATE SET
              override_kind = EXCLUDED.override_kind,
              custom_model_ratio = EXCLUDED.custom_model_ratio,
              custom_completion_ratio = EXCLUDED.custom_completion_ratio,
              custom_cache_ratio = EXCLUDED.custom_cache_ratio,
              custom_cache_write_ratio = EXCLUDED.custom_cache_write_ratio,
              custom_input_per_1m_micro = EXCLUDED.custom_input_per_1m_micro,
              custom_output_per_1m_micro = EXCLUDED.custom_output_per_1m_micro,
              reason = EXCLUDED.reason,
              expires_at = EXCLUDED.expires_at
          RETURNING id",
    )
    .bind(user_id)
    .bind(model_id)
    .bind(&req.override_kind)
    .bind(req.custom_model_ratio.as_deref().map(str::trim))
    .bind(req.custom_completion_ratio.as_deref().map(str::trim))
    .bind(req.custom_cache_ratio.as_deref().map(str::trim))
    .bind(req.custom_cache_write_ratio.as_deref().map(str::trim))
    .bind(req.custom_input_per_1m_micro)
    .bind(req.custom_output_per_1m_micro)
    .bind(reason)
    .bind(req.expires_at)
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    audit(
        &state,
        &actor,
        "pricing.upsert_user_override",
        &format!("{user_id}/{model_name}"),
        json!({ "id": id, "override_kind": req.override_kind, "reason": reason }),
    )
    .await;
    Ok(Json(json!({ "id": id, "requires_publish": true })))
}

pub async fn delete_user_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((user_id, model_name)): Path<(i64, String)>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let affected = sqlx::query(
        "DELETE FROM user_pricing up USING models m \
         WHERE up.model_id = m.id AND up.user_id = $1 AND m.model_name = $2",
    )
    .bind(user_id)
    .bind(model_name.trim())
    .execute(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    audit(
        &state,
        &actor,
        "pricing.delete_user_override",
        &format!("{user_id}/{model_name}"),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true, "requires_publish": true })))
}
