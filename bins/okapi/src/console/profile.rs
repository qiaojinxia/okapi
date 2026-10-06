//! Session-owned profile editing. API keys cannot change their owner's identity.
use crate::gateway::{
    auth::authenticate, error::AppError, extract::Json as ExtractJson, state::AppState,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(sqlx::FromRow, Serialize)]
struct Profile {
    username: String,
    email: Option<String>,
    language: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn owner(state: &AppState, headers: &HeaderMap) -> Result<i64, AppError> {
    let key = authenticate(state, headers).await?;
    let user_id = super::auth_web::require_session(state, headers).await?;
    if user_id != key.actor_user_id() {
        return Err(AppError::unauthorized(okapi_api::codes::INVALID_API_KEY));
    }
    Ok(user_id)
}

pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user_id = owner(&state, &headers).await?;
    let row = sqlx::query_as::<_, Profile>("SELECT username,email,language,created_at FROM users WHERE id=$1 AND deleted_at IS NULL AND status=1 AND kind='user'")
        .bind(user_id).fetch_optional(&state.pg).await.map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))?;
    Ok(Json(json!(row)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProfile {
    username: String,
    language: String,
}

fn valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.chars().count() <= 64
        && !username.chars().any(char::is_control)
}

pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpdateProfile>,
) -> Result<Json<Value>, AppError> {
    let user_id = owner(&state, &headers).await?;
    let username = req.username.trim();
    if !valid_username(username) {
        return Err(AppError::bad_request().with_param("username"));
    }
    if !matches!(req.language.as_str(), "auto" | "zh-CN" | "en") {
        return Err(AppError::bad_request().with_param("language"));
    }
    let row = sqlx::query_as::<_, Profile>("UPDATE users SET username=$2,language=$3,updated_at=now() WHERE id=$1 AND deleted_at IS NULL AND status=1 AND kind='user' RETURNING username,email,language,created_at")
        .bind(user_id).bind(username).bind(&req.language).fetch_optional(&state.pg).await
        .map_err(|error| {
            if error.as_database_error().is_some_and(sqlx::error::DatabaseError::is_unique_violation) {
                AppError::new(StatusCode::CONFLICT, "profile_username_taken")
            } else { okapi_store::StoreError::from(error).into() }
        })?
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))?;
    if let Err(error) = okapi_store::admin::record_audit(
        &state.pg,
        &format!("user:{user_id}"),
        "user.profile_update",
        &user_id.to_string(),
        json!({"username": username,"language": req.language}),
    )
    .await
    {
        tracing::error!(%error, "profile audit write failed");
    }
    Ok(Json(json!(row)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_unicode_length_and_rejects_hidden_controls() {
        assert!(valid_username("中文用户"));
        assert!(valid_username(&"中".repeat(64)));
        assert!(!valid_username(&"中".repeat(65)));
        assert!(!valid_username(""));
        assert!(!valid_username("name\nspoof"));
    }
    #[test]
    fn cannot_supply_another_user_or_privileged_fields() {
        assert!(
            serde_json::from_value::<UpdateProfile>(
                json!({"username":"alice","language":"en","user_id":2})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<UpdateProfile>(
                json!({"username":"alice","language":"en","role":100})
            )
            .is_err()
        );
    }
}
