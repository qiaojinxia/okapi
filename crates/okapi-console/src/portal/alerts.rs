//! 用户级自助告警偏好：个人低余额阈值、API key 即将到期提前提醒、邮件/Webhook 通道。
//! 仅主账户 Web Session 可读写（API key 不得篡改属主的告警目标或借 Webhook 探测内网）。
use crate::console_auth::authenticate;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use serde::Deserialize;
use serde_json::{Value, json};

const DEFAULT_MIN_INTERVAL_SECS: i32 = 3600;
const MIN_INTERVAL_SECS: i32 = 60;
const MAX_INTERVAL_SECS: i32 = 604_800;

#[derive(sqlx::FromRow)]
struct AlertPrefRow {
    low_balance_threshold_micro: Option<i64>,
    key_expiry_warn_days: Option<i32>,
    email_enabled: bool,
    webhook_url: Option<String>,
    webhook_secret: Option<String>,
    min_interval_secs: i32,
    updated_at: chrono::DateTime<chrono::Utc>,
}

async fn owner_session(state: &AppState, headers: &HeaderMap) -> Result<i64, AppError> {
    let key = authenticate(state, headers).await?;
    let user_id = super::super::auth_web::require_session(state, headers).await?;
    if user_id != key.actor_user_id() {
        return Err(AppError::unauthorized(okapi_api::codes::INVALID_API_KEY));
    }
    Ok(user_id)
}

fn serialize_pref(row: Option<AlertPrefRow>) -> Value {
    match row {
        Some(r) => json!({
            "low_balance_threshold_micro": r.low_balance_threshold_micro,
            "key_expiry_warn_days": r.key_expiry_warn_days,
            "email_enabled": r.email_enabled,
            "webhook_url": r.webhook_url,
            "webhook_secret_configured": r.webhook_secret.as_deref().is_some_and(|s| !s.is_empty()),
            "min_interval_secs": r.min_interval_secs,
            "updated_at": r.updated_at.to_rfc3339(),
        }),
        None => json!({
            "low_balance_threshold_micro": Value::Null,
            "key_expiry_warn_days": Value::Null,
            "email_enabled": false,
            "webhook_url": Value::Null,
            "webhook_secret_configured": false,
            "min_interval_secs": DEFAULT_MIN_INTERVAL_SECS,
            "updated_at": Value::Null,
        }),
    }
}

/// GET /api/me/alerts：读取当前用户的告警偏好（密钥不回显明文）。
pub async fn get_alerts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user_id = owner_session(&state, &headers).await?;
    let row = sqlx::query_as::<_, AlertPrefRow>(
        "SELECT low_balance_threshold_micro, key_expiry_warn_days, email_enabled, \
                webhook_url, webhook_secret, min_interval_secs, updated_at \
         FROM user_alert_prefs WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(Json(serialize_pref(row)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutAlertsReq {
    #[serde(default)]
    pub low_balance_threshold_micro: Option<i64>,
    #[serde(default)]
    pub key_expiry_warn_days: Option<i32>,
    #[serde(default)]
    pub email_enabled: bool,
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// 缺席 = 保留现有 secret；null / 空串 = 清除；非空字符串 = 更新。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub webhook_secret: Option<Option<String>>,
    #[serde(default)]
    pub min_interval_secs: Option<i32>,
}

#[allow(clippy::option_option)]
fn normalize_secret(raw: Option<Option<String>>) -> Result<(bool, Option<String>), AppError> {
    match raw {
        None => Ok((false, None)),
        Some(None) => Ok((true, None)),
        Some(Some(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok((true, None))
            } else if trimmed.len() > 256 || trimmed.chars().any(char::is_control) {
                Err(AppError::bad_request().with_param("webhook_secret"))
            } else {
                Ok((true, Some(trimmed.to_owned())))
            }
        }
    }
}

/// PUT /api/me/alerts：保存个人告警偏好；Webhook 走全站 SSRF 策略校验，邮箱开关要求已绑定邮箱。
pub async fn put_alerts(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<PutAlertsReq>,
) -> Result<Json<Value>, AppError> {
    let user_id = owner_session(&state, &headers).await?;
    super::validate_budget_cap(
        "low_balance_threshold_micro",
        req.low_balance_threshold_micro,
    )?;
    if req
        .key_expiry_warn_days
        .is_some_and(|days| !(1..=90).contains(&days))
    {
        return Err(AppError::bad_request().with_param("key_expiry_warn_days"));
    }
    let min_interval = req.min_interval_secs.unwrap_or(DEFAULT_MIN_INTERVAL_SECS);
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&min_interval) {
        return Err(AppError::bad_request().with_param("min_interval_secs"));
    }
    let webhook_url = req
        .webhook_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(url) = &webhook_url {
        if url.len() > 512 || url.chars().any(char::is_control) {
            return Err(AppError::bad_request().with_param("webhook_url"));
        }
        okapi_kernel::ssrf::validate_url(&state.pg, url).await?;
    }
    if req.email_enabled {
        let email: Option<String> = sqlx::query_scalar(
            "SELECT email FROM users WHERE id = $1 AND deleted_at IS NULL AND status = 1",
        )
        .bind(user_id)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .flatten();
        if email.as_deref().is_none_or(|e| e.trim().is_empty()) {
            return Err(AppError::bad_request().with_param("email_required"));
        }
    }
    let (update_secret, new_secret) = normalize_secret(req.webhook_secret)?;
    let row = sqlx::query_as::<_, AlertPrefRow>(
        r"INSERT INTO user_alert_prefs (
              user_id, low_balance_threshold_micro, key_expiry_warn_days,
              email_enabled, webhook_url, webhook_secret, min_interval_secs, updated_at
          )
          VALUES ($1, $2, $3, $4, $5, $6, $7, now())
          ON CONFLICT (user_id) DO UPDATE SET
              low_balance_threshold_micro = EXCLUDED.low_balance_threshold_micro,
              key_expiry_warn_days = EXCLUDED.key_expiry_warn_days,
              email_enabled = EXCLUDED.email_enabled,
              webhook_url = EXCLUDED.webhook_url,
              webhook_secret = CASE WHEN $8::bool THEN EXCLUDED.webhook_secret ELSE user_alert_prefs.webhook_secret END,
              min_interval_secs = EXCLUDED.min_interval_secs,
              updated_at = now()
          RETURNING low_balance_threshold_micro, key_expiry_warn_days, email_enabled,
                    webhook_url, webhook_secret, min_interval_secs, updated_at",
    )
    .bind(user_id)
    .bind(req.low_balance_threshold_micro)
    .bind(req.key_expiry_warn_days)
    .bind(req.email_enabled)
    .bind(webhook_url.as_deref())
    .bind(new_secret.as_deref())
    .bind(min_interval)
    .bind(update_secret)
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    if let Err(error) = okapi_store::admin::record_audit(
        &state.pg,
        &format!("user:{user_id}"),
        "user.alerts_update",
        &user_id.to_string(),
        json!({
            "low_balance_threshold_micro": row.low_balance_threshold_micro,
            "key_expiry_warn_days": row.key_expiry_warn_days,
            "email_enabled": row.email_enabled,
            "webhook_configured": row.webhook_url.is_some(),
            "min_interval_secs": row.min_interval_secs,
        }),
    )
    .await
    {
        tracing::error!(%error, "user alerts audit write failed");
    }
    Ok(Json(serialize_pref(Some(row))))
}
