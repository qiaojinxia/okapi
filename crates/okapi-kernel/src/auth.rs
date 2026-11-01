//! 鉴权（IMPLEMENTATION §11.58）：API key（Bearer / x-api-key / x-goog-api-key）→ 鉴权缓存 → PG 回源；
//! 会话绑定的登录 key 要带着所属会话的 cookie；缺 key / 未命中按来源 IP 计数，超限 429。
//!
//! 只用到各角色共用的状态，三个角色都从这里鉴权。数据面另加的准入（结算积压泄压、key 级 IP 白名单）
//! 在 `gateway::auth::authenticate_data_plane`。
use super::error::AppError;
use super::state::CoreState;
use axum::http::HeaderMap;
use okapi_api::codes;
use okapi_store::AuthedKey;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Bearer 鉴权：SHA-256(key) → Redis 缓存（auth:key:*，60s + 版本失效）→ PG 回源。
/// 缓存在 Redis 而非进程内：console 的角色/分组变更经 auth:ver 跨进程立即失效
/// （docs/database.md §2.3/§2.4）。
pub async fn authenticate(
    state: &CoreState,
    headers: &HeaderMap,
) -> Result<Arc<AuthedKey>, AppError> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    // Anthropic 协议客户端（Claude Code 等）用 x-api-key 头；Gemini SDK 用 x-goog-api-key
    // （`?key=` 查询串由 gemini 入口搬进该头）
    let token = bearer
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .or_else(|| headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()))
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let Some(token) = token else {
        return Err(reject_invalid_key(state, headers).await);
    };

    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let authed = if let Some(hit) = state.sched.auth_get(&key_hash).await {
        Arc::new(hit)
    } else {
        let mut resolved = None;
        for _ in 0..3 {
            let version = state.sched.auth_version().await;
            let found = okapi_store::auth::find_key_by_hash(&state.pg, &key_hash).await?;
            let Some(found) = found else {
                return Err(reject_invalid_key(state, headers).await);
            };
            if let Some(version) = version {
                if state.sched.auth_version().await != Some(version) {
                    continue;
                }
                state.sched.auth_set(&key_hash, &found, version).await;
            }
            resolved = Some(Arc::new(found));
            break;
        }
        resolved.ok_or_else(|| {
            AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                codes::OVERLOADED,
            )
        })?
    };

    if !authed.is_usable(chrono::Utc::now()) {
        return Err(AppError::unauthorized(codes::KEY_DISABLED));
    }
    // 登录 key 跟着登录会话走（§6.4）：请求必须带着它所属会话的 cookie，且会话仍有效（顺带续期）。
    // 会话结束——退出、被踢、全部吊销、改密码、超出在线上限、过期——它随之失效；被偷到别处也用不了。
    if let Some(bound) = authed.session_hash.as_deref() {
        match session_alive(state, headers, bound, authed.user_id).await {
            Ok(true) => {}
            Ok(false) => return Err(AppError::unauthorized(codes::INVALID_API_KEY)),
            // 会话存储不可达是服务端故障：报 503，不伪装成凭证无效把排障引向安全事件
            Err(error) => {
                tracing::error!(%error, "web session store unreachable while authorizing a session-bound key");
                return Err(AppError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    codes::AUTH_BACKEND_UNAVAILABLE,
                ));
            }
        }
    }
    Ok(authed)
}

/// web 会话 cookie 名（门户 / 管理后台登录后由 `console::auth_web` 下发）。
pub const SESSION_COOKIE: &str = "okapi_session";

/// Cookie 头解析会话 id：控制台按它找会话，网关校验会话绑定的登录 key 时也用。
pub fn session_id(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == SESSION_COOKIE).then(|| value.to_owned())
    })
}

async fn session_alive(
    state: &CoreState,
    headers: &HeaderMap,
    bound: &str,
    user_id: i64,
) -> Result<bool, fred::error::Error> {
    let Some(sid) = session_id(headers) else {
        return Ok(false);
    };
    if hex::encode(Sha256::digest(sid.as_bytes())) != bound {
        return Ok(false);
    }
    Ok(state.sched.web_session_check(&sid).await? == Some(user_id))
}

/// 无效凭证计数（缺 token / 哈希未命中）。超限改回 429，避免扫 key 打满 401。
/// 鉴权成功后的模型 / IP 白名单失败不计——那是授权，不是猜密钥。
async fn reject_invalid_key(state: &CoreState, headers: &HeaderMap) -> AppError {
    let limit = state
        .setting_cached("critical_rate_limits")
        .await
        .as_ref()
        .as_ref()
        .and_then(|v| v.get("invalid_api_key"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(60);
    if limit > 0
        && let Some(ip) = super::clients::detect_client_ip(headers)
    {
        let count = state.sched.crit_rate_incr("invalid_api_key", &ip).await;
        if count > limit {
            return AppError::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                codes::RATE_LIMITED,
            )
            .with_param("invalid_api_key");
        }
    }
    AppError::unauthorized(codes::INVALID_API_KEY)
}
