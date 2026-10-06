//! Explicit owner-session secret retrieval. Never expose secrets in key lists or logs.
use super::auth_web::{MaybeConnectInfo, critical_rate_guard, require_session};
use crate::gateway::{error::AppError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

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
    let key = crate::gateway::auth::authenticate(state, headers).await?;
    if key.actor_user_id() != user_id {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "key_copy_session_mismatch",
        ));
    }
    critical_rate_guard(state, headers, conn.0.as_ref(), "key_copy", 30).await?;
    let row: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT key_hash, key_ciphertext FROM api_keys WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL"
    ).bind(id).bind(user_id).fetch_optional(&state.pg).await.map_err(okapi_store::StoreError::from)?;
    let (hash, ciphertext) =
        row.ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "not_found"))?;
    let ciphertext =
        ciphertext.ok_or_else(|| AppError::new(StatusCode::CONFLICT, "key_copy_not_saved"))?;
    let master = state
        .master_key
        .as_deref()
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "key_copy_unavailable"))?;
    let token = okapi_store::api_key_secret::open(master, user_id, &hash, &ciphertext)
        .map_err(|_| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "key_copy_unavailable"))?;
    Ok(Json(json!({ "api_key": token })))
}
