//! Setup 初始化向导（IMPLEMENTATION §13 M3 前端配套）：
//! 空库首启创建超管 + 首个 API key（明文仅返回一次）。
//! 排他性：事务级表锁保证并发首启只成功一次；已初始化恒 409。
//! 单用户模式（OKAPI_SINGLE_USER_MODE，§6.5）是另一条免注册路径，两者互不依赖。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// GET /api/setup/status：users 表空 = 待初始化。
pub async fn status(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let users = sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM users"#)
        .fetch_one(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({ "needs_setup": users == 0 })))
}

#[derive(Deserialize)]
pub struct SetupReq {
    pub username: String,
}

/// POST /api/setup：创建超管（role=100）与首个 key。
pub async fn run(
    State(state): State<AppState>,
    Json(req): Json<SetupReq>,
) -> Result<Json<Value>, AppError> {
    let username = req.username.trim();
    if username.is_empty() || username.len() > 64 {
        return Err(AppError::bad_request().with_param("username"));
    }

    let secret: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(43)
        .map(char::from)
        .collect();
    let token = format!("sk-okapi-{secret}");
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let key_prefix: String = token.chars().take(16).collect();

    let created =
        okapi_store::provision::setup_first_admin(&state.pg, username, &key_hash, &key_prefix)
            .await?;
    let Some((user_id, key_id)) = created else {
        return Err(AppError::new(StatusCode::CONFLICT, "already_initialized"));
    };
    tracing::warn!(user_id, "Setup 向导：超管已创建（key 明文仅本次返回）");
    Ok(Json(json!({
        "user_id": user_id,
        "key_id": key_id,
        // 唯一一次明文返回（前端提示立即保存）
        "api_key": token,
    })))
}
