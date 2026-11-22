//! Explicit owner-session secret retrieval. Never expose secrets in key lists or logs.
use super::auth_web::{MaybeConnectInfo, critical_rate_guard, require_session};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use serde_json::{Value, json};

#[derive(sqlx::FromRow)]
struct CopyKeyRow {
    key_hash: String,
    key_ciphertext: Option<Vec<u8>>,
    key_prefix: String,
    name: Option<String>,
}

pub async fn copy(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    let result = read(&state, id, conn, &headers).await;
    let mut response = result.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store, private".parse().unwrap());
    response
        .headers_mut()
        .insert(header::PRAGMA, "no-cache".parse().unwrap());
    response
}

async fn read(
    state: &AppState,
    id: i64,
    conn: MaybeConnectInfo,
    headers: &HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user_id = require_session(state, headers).await?;
    // A lingering cookie for a different account must not silently switch owners.
    let key = crate::console_auth::authenticate(state, headers).await?;
    if key.actor_user_id() != user_id {
        super::audit::record_event(
            state,
            &format!("user:{user_id}"),
            "apikey.copy_denied",
            &id.to_string(),
            json!({
                "reason": "session_mismatch",
                "session_user_id": user_id,
                "key_user_id": key.actor_user_id(),
            }),
            Some(key.key_id),
        )
        .await;
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "key_copy_session_mismatch",
        ));
    }
    critical_rate_guard(state, headers, conn.0.as_ref(), "key_copy", 30).await?;
    let row: Option<CopyKeyRow> = sqlx::query_as(
        "SELECT key_hash, key_ciphertext, key_prefix, name FROM api_keys WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let CopyKeyRow {
        key_hash: hash,
        key_ciphertext: ciphertext,
        key_prefix,
        name: key_name,
    } = row.ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let ciphertext =
        ciphertext.ok_or_else(|| AppError::new(StatusCode::CONFLICT, "key_copy_not_saved"))?;
    let master = state
        .master_key
        .as_deref()
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "key_copy_unavailable"))?;
    let token = okapi_store::api_key_secret::open(master, user_id, &hash, &ciphertext)
        .map_err(|_| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "key_copy_unavailable"))?;
    super::audit::record_event(
        state,
        &format!("user:{user_id}"),
        "apikey.copy",
        &id.to_string(),
        json!({ "key_prefix": key_prefix, "name": key_name }),
        Some(key.key_id),
    )
    .await;
    Ok(Json(json!({ "api_key": token })))
}
