//! web 会话：登录、会话 cookie、退出，以及会话列表与吊销。
use super::{
    MaybeConnectInfo, SESSION_COOKIE, critical_rate_guard, rand_token, session_hash, session_id,
};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use crate::kernel::extract::Path;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okapi_store::identity;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// ---- 登录 / 登出 ----

#[derive(Deserialize)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub totp_code: Option<String>,
}

pub async fn login(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<LoginReq>,
) -> Result<Response, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "login", 10).await?;
    let email = req.email.trim().to_lowercase();
    let ip = crate::kernel::clients::detect_client_ip(&headers)
        .or_else(|| conn.0.as_ref().map(|a| a.ip().to_string()));

    // 凭证与 TOTP 校验单独成段：成功与失败都要落审计（§3.5 登录 = 审计 user.login），
    // 失败原因只进审计，对客户端仍是同一个 401
    let verified = verify_login(&state, &email, &req).await;
    let user = match verified {
        Ok(user) => user,
        Err((reason, err)) => {
            super::super::audit::record_login(
                &state,
                &email,
                None,
                false,
                Some(reason),
                ip,
                &headers,
            )
            .await;
            return Err(err);
        }
    };
    super::super::audit::record_login(
        &state,
        &email,
        Some(user.user_id),
        true,
        None,
        ip.clone(),
        &headers,
    )
    .await;

    let sid = open_web_session(&state, user.user_id, ip.as_deref(), &headers).await;
    let mut resp = Json(json!({ "user_id": user.user_id, "role": user.role })).into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&session_cookie(&sid)) {
        resp.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(resp)
}

/// 建一条 web 会话并按 `settings.web_session_limit` 裁剪该用户的旧会话（§11.37）。
/// 登录与 OAuth 回调都经这里：上限只有一处生效点，刚建的这条永不被踢。
pub(in crate::console) async fn open_web_session(
    state: &AppState,
    user_id: i64,
    ip: Option<&str>,
    headers: &HeaderMap,
) -> String {
    let sid = crate::kernel::sched_redis::SchedulerRedis::web_session_sid(user_id, &rand_token(48));
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    state.sched.web_session_set(&sid, user_id, ip, ua).await;
    let limit = web_session_limit(state).await;
    if limit > 0 {
        let evicted = state.sched.web_session_trim(user_id, limit, &sid).await;
        if evicted > 0 {
            tracing::info!(user_id, evicted, limit, "web 会话超上限，已踢最早的会话");
        }
    }
    sid
}

/// 未配置 `settings.web_session_limit` 时的同时在线会话数。
///
/// 此前缺省是 0（不限），于是没人配就永不淘汰：每次登录、每次清 cookie、每个
/// 测试脚本都留下一条，安全页的"有效登录会话"能长到几十行——用户既认不出哪条
/// 是自己的，也就不会去吊销可疑的那条，这张卡等于白做。活跃会话越多攻击面越大，
/// 主流站点（GitHub / Google）都封顶并挤掉最早的，这里跟齐。
pub(super) const DEFAULT_WEB_SESSION_LIMIT: i64 = 10;

/// `settings.web_session_limit`：未配置 = [`DEFAULT_WEB_SESSION_LIMIT`]，
/// **显式配 0 仍是"不限"**——保留这个逃生口，但它得是站长主动选的。
pub(in crate::console) async fn web_session_limit(state: &AppState) -> i64 {
    state
        .setting_cached("web_session_limit")
        .await
        .as_ref()
        .as_ref()
        .and_then(serde_json::Value::as_i64)
        .filter(|v| *v >= 0)
        .unwrap_or(DEFAULT_WEB_SESSION_LIMIT)
}

/// 会话 cookie（HttpOnly，7 天，与 `sess:web` TTL 对齐）。
pub(in crate::console) fn session_cookie(sid: &str) -> String {
    session_cookie_with_security(sid, secure_cookie())
}

pub(in crate::console) fn secure_cookie() -> bool {
    static SECURE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SECURE.get_or_init(|| {
        let secure = okapi_store::env_config::flag("OKAPI_COOKIE_SECURE").unwrap_or(true);
        if !secure {
            tracing::warn!("Secure cookies explicitly disabled for HTTP deployment");
        }
        secure
    })
}

pub(super) fn session_cookie_with_security(sid: &str, secure: bool) -> String {
    let flag = if secure { "; Secure" } else { "" };
    format!("{SESSION_COOKIE}={sid}{flag}; HttpOnly; SameSite=Lax; Path=/; Max-Age=604800")
}

/// 密码 + TOTP 校验；`Err((审计原因, 对外错误))`。
pub(super) async fn verify_login(
    state: &AppState,
    email: &str,
    req: &LoginReq,
) -> Result<identity::LoginUser, (&'static str, AppError)> {
    let user = identity::find_login_user(&state.pg, email, &req.password)
        .await
        .map_err(|e| ("store_error", AppError::from(e)))?
        .ok_or_else(|| {
            (
                "invalid_credentials",
                AppError::unauthorized("invalid_credentials"),
            )
        })?;

    if user.totp_enabled {
        let Some(code) = req.totp_code.as_deref() else {
            return Err(("totp_required", AppError::unauthorized("totp_required")));
        };
        let master = state.master_key.as_deref().ok_or_else(|| {
            (
                "totp_disabled",
                AppError::new(StatusCode::NOT_IMPLEMENTED, "totp_disabled"),
            )
        })?;
        let sealed = user
            .totp_secret_ciphertext
            .as_deref()
            .ok_or_else(|| ("totp_secret_missing", AppError::internal()))?;
        let secret = identity::open_totp_secret(master, sealed)
            .map_err(|_| ("totp_secret_unreadable", AppError::internal()))?;
        let counter =
            identity::matching_totp_counter(&secret, code, chrono::Utc::now().timestamp());
        if let Some(counter) = counter {
            if !identity::consume_totp(&state.pg, user.user_id, sealed, counter)
                .await
                .map_err(|_| ("totp_store", AppError::internal()))?
            {
                return Err(("totp_invalid", AppError::unauthorized("totp_invalid")));
            }
        } else {
            return Err(("totp_invalid", AppError::unauthorized("totp_invalid")));
        }
    }
    Ok(user)
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    if let Some(sid) = session_id(&headers) {
        state.sched.web_session_del(&sid).await;
        // 这条会话换来的登录 key 本就随会话失效了；这里把它也删掉，不留死 key
        let revoked = okapi_store::admin::soft_delete_session_keys(&state.pg, &session_hash(&sid))
            .await
            .unwrap_or_else(|error| {
                // 会话已删，绑定它的 key 鉴权时也过不了；只是行没删掉，留痕好查
                tracing::error!(%error, "退出登录时作废会话绑定的登录 key 失败");
                Vec::new()
            });
        for hash in &revoked {
            state.sched.auth_del(hash).await;
        }
    }
    Json(json!({ "ok": true }))
}

/// 吊销 web 会话是安全相关的写操作：留一条审计，记下是哪把 key 把哪个会话踢掉的
/// （门户 API key 鉴权下，持 key 的一方就能做这件事，事后要能查到）。
pub(super) async fn audit_session_revoke(
    state: &AppState,
    key: &okapi_store::auth::AuthedKey,
    action: &str,
    target: &str,
) {
    if let Err(error) = okapi_store::admin::record_audit(
        &state.pg,
        &format!("user:{}", key.user_id),
        action,
        target,
        json!({"key_id": key.key_id}),
    )
    .await
    {
        tracing::error!(%error, action, "会话吊销审计写入失败");
    }
}

/// GET /api/me/sessions：当前用户仍有效的 web 会话（门户 API key 鉴权）。
pub async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    let current = session_id(&headers);
    let data: Vec<Value> = state
        .sched
        .web_session_list(key.user_id)
        .await
        .into_iter()
        .map(|s| {
            json!({
                "sid": session_fingerprint(&s.sid),
                "ip": s.ip,
                "ua": s.ua,
                "created_at": s.created_at,
                "current": current.as_deref() == Some(s.sid.as_str()),
            })
        })
        .collect();
    // 上限回给前端显示"最多同时 N 个"；0 = 不限回 null
    let limit = web_session_limit(&state).await;
    Ok(Json(
        json!({ "data": data, "limit": (limit > 0).then_some(limit) }),
    ))
}

/// DELETE /api/me/sessions/{sid}
pub async fn revoke_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    if sid.len() != 32 || !sid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::bad_request().with_param("session"));
    }
    let session = state
        .sched
        .web_session_list(key.user_id)
        .await
        .into_iter()
        .find(|s| session_fingerprint(&s.sid) == sid);
    if let Some(session) = session {
        if !state
            .sched
            .web_session_revoke(key.user_id, &session.sid)
            .await
        {
            return Err(AppError::new(
                StatusCode::NOT_FOUND,
                okapi_api::codes::NOT_FOUND,
            ));
        }
    } else {
        return Err(
            AppError::new(StatusCode::NOT_FOUND, okapi_api::codes::NOT_FOUND).with_param("session"),
        );
    }
    audit_session_revoke(&state, &key, "user.session_revoke", &sid).await;
    Ok(Json(json!({ "ok": true })))
}

/// Public revocation handle; never usable as a login cookie.
pub(super) fn session_fingerprint(sid: &str) -> String {
    hex::encode(Sha256::digest(sid.as_bytes()))[..32].to_owned()
}

/// DELETE /api/me/sessions：吊销该用户全部 web 会话。
pub async fn revoke_all_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    state.sched.web_session_revoke_user(key.user_id).await;
    audit_session_revoke(
        &state,
        &key,
        "user.session_revoke_all",
        &key.user_id.to_string(),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod cookie_tests {
    use super::*;
    #[test]
    fn http_escape_hatch_keeps_other_cookie_protections() {
        for secure in [true, false] {
            let cookie = session_cookie_with_security("sid", secure);
            assert_eq!(cookie.contains("; Secure"), secure);
            assert!(cookie.contains("HttpOnly; SameSite=Lax; Path=/"));
        }
    }
}
