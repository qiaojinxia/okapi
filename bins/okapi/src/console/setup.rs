//! Setup 初始化向导（IMPLEMENTATION §13 M3 前端配套）：
//! 空库首启创建超管 + 首个 API key（明文仅返回一次）。
//! 排他性：事务级表锁保证并发首启只成功一次；已初始化恒 409。
//! 单用户模式（OKAPI_SINGLE_USER_MODE，§6.5）是另一条免注册路径，两者互不依赖。

use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
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
    #[serde(default)]
    pub setup_token: Option<String>,
}

/// POST /api/setup：创建超管（role=100）与首个 key。
pub async fn run(
    State(state): State<AppState>,
    conn: super::auth_web::MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<SetupReq>,
) -> Result<Json<Value>, AppError> {
    super::auth_web::critical_rate_guard(&state, &headers, conn.0.as_ref(), "setup", 5).await?;
    let configured = std::env::var("OKAPI_SETUP_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let authorized = if let Some(expected) = configured {
        req.setup_token.as_deref().is_some_and(|provided| {
            provided.len() == expected.len()
                && provided
                    .bytes()
                    .zip(expected.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
        })
    } else {
        direct_loopback(conn.0, &headers)
    };
    if !authorized {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, okapi_api::codes::PERMISSION_DENIED)
                .with_param("setup_token_required"),
        );
    }
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

fn direct_loopback(peer: Option<std::net::SocketAddr>, headers: &HeaderMap) -> bool {
    peer.is_some_and(|peer| peer.ip().is_loopback())
        && !headers.keys().any(|name| {
            name.as_str().starts_with("x-forwarded-")
                || matches!(
                    name.as_str(),
                    "forwarded" | "x-real-ip" | "true-client-ip" | "cf-connecting-ip"
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_loopback_never_grants_bootstrap_access() {
        let local = Some("127.0.0.1:1234".parse().unwrap());
        assert!(direct_loopback(local, &HeaderMap::new()));
        assert!(!direct_loopback(None, &HeaderMap::new()));
        assert!(!direct_loopback(
            Some("203.0.113.1:1234".parse().unwrap()),
            &HeaderMap::new()
        ));
        for name in [
            "forwarded",
            "x-forwarded-for",
            "x-forwarded-host",
            "x-real-ip",
            "cf-connecting-ip",
            "true-client-ip",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(name, "127.0.0.1".parse().unwrap());
            assert!(!direct_loopback(local, &headers), "{name}");
        }
    }
}
