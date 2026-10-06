//! Actor-aware user management shared by HTTP and MCP.
use crate::gateway::{error::AppError, state::AppState};
use axum::http::StatusCode;
use okapi_api::codes;
use okapi_store::{
    auth::AuthedKey,
    mutate::{self, UserAction},
};

pub(super) async fn validate(
    state: &AppState,
    actor: &AuthedKey,
    target: i64,
    action: UserAction,
) -> Result<(), AppError> {
    if target == actor.actor_user_id() {
        return Err(AppError::bad_request().with_param("self_target"));
    }
    if matches!(action, UserAction::Promote | UserAction::Demote) && actor.role < 100 {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("super_admin_required"),
        );
    }
    let role: i16 = sqlx::query_scalar("SELECT role FROM users WHERE id=$1 AND deleted_at IS NULL")
        .bind(target)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    if role >= 100 {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("super_admin_protected"),
        );
    }
    Ok(())
}

pub(super) async fn apply(
    state: &AppState,
    actor: &AuthedKey,
    target: i64,
    action: UserAction,
) -> Result<(), AppError> {
    validate(state, actor, target, action).await?;
    // The store repeats the protected-role predicate in the actual UPDATE.
    if !mutate::manage_user(&state.pg, target, action).await? {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    if matches!(action, UserAction::Ban | UserAction::Delete) {
        state.sched.web_session_revoke_user(target).await;
    }
    state.sched.auth_flush().await;
    Ok(())
}
