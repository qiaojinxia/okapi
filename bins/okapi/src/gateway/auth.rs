use super::error::AppError;
use super::state::AppState;
use axum::http::HeaderMap;
use okapi_api::codes;
use okapi_store::AuthedKey;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Bearer 鉴权：SHA-256(key) → Redis 缓存（auth:key:*，60s + 版本失效）→ PG 回源。
/// 缓存在 Redis 而非进程内：console 的角色/分组变更经 auth:ver 跨进程立即失效
/// （docs/database.md §2.3/§2.4）。
pub async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<AuthedKey>, AppError> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    // Anthropic 协议客户端（Claude Code 等）用 x-api-key 头
    let token = bearer
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| AppError::unauthorized(codes::INVALID_API_KEY))?;

    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let authed = if let Some(hit) = state.sched.auth_get(&key_hash).await {
        Arc::new(hit)
    } else {
        let found = okapi_store::auth::find_key_by_hash(&state.pg, &key_hash)
            .await?
            .ok_or_else(|| AppError::unauthorized(codes::INVALID_API_KEY))?;
        state.sched.auth_set(&key_hash, &found).await;
        Arc::new(found)
    };

    if !authed.is_usable(chrono::Utc::now()) {
        return Err(AppError::unauthorized(codes::KEY_DISABLED));
    }
    Ok(authed)
}

/// 团成员月度限额检查（软实时：结算后计数、预扣前比较，§6.1）。
/// 各计费端点在 reserve 前调用；非团 key / 未配限额直接放行。
pub async fn check_member_limit(state: &AppState, key: &AuthedKey) -> Result<(), AppError> {
    let (Some(member), Some(limit)) = (key.member_user_id, key.member_monthly_limit_micro) else {
        return Ok(());
    };
    let spent = state.sched.member_spend_get(key.user_id, member).await;
    if spent >= limit {
        return Err(AppError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            codes::MEMBER_LIMIT_EXCEEDED,
        ));
    }
    Ok(())
}

/// 结算后累计成员消费（团 key 才计）。
pub async fn record_member_spend(
    state: &AppState,
    key_user_id: i64,
    member: Option<i64>,
    amount_micro: i64,
) {
    if let Some(member) = member {
        state
            .sched
            .member_spend_add(key_user_id, member, amount_micro)
            .await;
    }
}
