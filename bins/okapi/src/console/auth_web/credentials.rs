//! 登录后的凭据：会话 key、TOTP 两步验证、门户自建 API key。
use super::{
    MaybeConnectInfo, critical_rate_guard, rand_token, require_session, session_hash, session_id,
    session_user,
};
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okapi_store::identity;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Deserialize, Default)]
pub struct SessionKeyReq {
    /// 只作展示（日志里分得出网页登录还是 OAuth 着陆）；缺省 web。
    #[serde(default)]
    pub name: Option<String>,
}

/// POST /auth/session-key：用当前登录会话换一把绑定它的登录 key（网页登录 / 注册 / OAuth 着陆后调用）。
///
/// 登录 key 只在请求带着这条会话的 cookie、且会话仍有效时可用：会话结束（退出、被踢、全部吊销、
/// 改密码、超出在线上限、过期）它随之失效，被偷到别处也用不了。同一会话重复兑换作废上一把
/// （明文只在这一刻返回，也不存可复制的密文）；顺手作废该用户已失效会话留下的登录 key。
/// 门户密钥列表不显示登录 key——它们在「登录设备」里随会话管理。
pub async fn session_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<SessionKeyReq>>,
) -> Result<Response, AppError> {
    let unauthorized = || AppError::unauthorized(okapi_api::codes::INVALID_API_KEY);
    let sid = session_id(&headers).ok_or_else(unauthorized)?;
    let user_id = session_user(&state, &sid).await?;
    let bound = session_hash(&sid);
    let name = body
        .and_then(|Json(req)| req.name)
        .map(|n| n.trim().to_owned())
        .filter(|n| (1..=32).contains(&n.chars().count()))
        .unwrap_or_else(|| "web".to_owned());
    // 仍有效的会话；列表里连本会话都没有说明 Redis 读出了问题，那就只作废本会话的上一把
    let mut live: Vec<String> = state
        .sched
        .web_session_list(user_id)
        .await
        .iter()
        .map(|s| session_hash(&s.sid))
        .collect();
    if !live.contains(&bound) {
        live.clear();
    }
    let token = format!("sk-okapi-{}", rand_token(43));
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let revoked: Vec<String> = sqlx::query_scalar(
        "UPDATE api_keys SET deleted_at = now() \
         WHERE user_id = $1 AND session_hash IS NOT NULL AND deleted_at IS NULL \
           AND (session_hash = $2 OR (cardinality($3::text[]) > 0 AND NOT (session_hash = ANY($3)))) \
         RETURNING key_hash",
    )
    .bind(user_id)
    .bind(&bound)
    .bind(&live)
    .fetch_all(&mut *tx)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let key_id: i64 = sqlx::query_scalar(
        "INSERT INTO api_keys (user_id, key_hash, key_prefix, name, session_hash) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user_id)
    .bind(&key_hash)
    .bind(token.chars().take(16).collect::<String>())
    .bind(&name)
    .bind(&bound)
    .fetch_one(&mut *tx)
    .await
    .map_err(okapi_store::StoreError::from)?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    for hash in &revoked {
        state.sched.auth_del(hash).await;
    }
    let mut response = Json(json!({ "key_id": key_id, "api_key": token })).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store, private".parse().unwrap());
    Ok(response)
}

// ---- TOTP 两段式注册 ----

#[derive(Deserialize)]
pub struct TotpPasswordReq {
    pub password: String,
}

pub(super) fn totp_binding(
    headers: &HeaderMap,
    user_id: i64,
    pending: &str,
) -> Result<String, AppError> {
    let sid = session_id(headers)
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))?;
    if pending.len() != 32 || !pending.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(AppError::bad_request().with_param("pending"));
    }
    Ok(format!(
        "{user_id}:{}:{pending}",
        hex::encode(Sha256::digest(sid.as_bytes()))
    ))
}

pub async fn totp_enroll(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<TotpPasswordReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "totp", 10).await?;
    let user_id = require_session(&state, &headers).await?;
    let user = identity::reauthenticate(&state.pg, user_id, &req.password)
        .await?
        .ok_or_else(|| AppError::unauthorized("invalid_credentials"))?;
    if user.totp_enabled {
        return Err(
            AppError::new(StatusCode::CONFLICT, okapi_api::codes::BAD_REQUEST)
                .with_param("totp_already_enabled"),
        );
    }
    let master = state
        .master_key
        .as_deref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, "totp_disabled"))?;
    let (secret, otpauth_url) = identity::generate_totp_secret(&user_id.to_string());
    let sealed = identity::seal_totp_secret(master, &secret).map_err(|_| AppError::internal())?;
    let pending = rand_token(32);
    let binding = totp_binding(&headers, user_id, &pending)?;
    if !state
        .sched
        .totp_pending_set(&binding, &hex::encode(sealed))
        .await
    {
        return Err(AppError::internal());
    }
    Ok(Json(json!({"otpauth_url":otpauth_url,"pending":pending})))
}

#[derive(Deserialize)]
pub struct TotpConfirmReq {
    pub pending: String,
    pub code: String,
}

pub async fn totp_confirm(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<TotpConfirmReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "totp", 10).await?;
    let user_id = require_session(&state, &headers).await?;
    let binding = totp_binding(&headers, user_id, &req.pending)?;
    let sealed = state
        .sched
        .totp_pending_get(&binding)
        .await
        .ok_or_else(|| AppError::bad_request().with_param("pending"))?;
    let sealed = hex::decode(sealed).map_err(|_| AppError::internal())?;
    let master = state
        .master_key
        .as_deref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, "totp_disabled"))?;
    let secret = identity::open_totp_secret(master, &sealed).map_err(|_| AppError::internal())?;
    let counter =
        identity::matching_totp_counter(&secret, &req.code, chrono::Utc::now().timestamp())
            .ok_or_else(|| AppError::bad_request().with_param("totp_code"))?;
    if !identity::enable_totp(&state.pg, user_id, &sealed, counter).await? {
        return Err(
            AppError::new(StatusCode::CONFLICT, okapi_api::codes::BAD_REQUEST)
                .with_param("totp_already_enabled"),
        );
    }
    state.sched.totp_pending_del(&binding).await;
    Ok(Json(json!({"enabled":true})))
}

#[derive(Deserialize)]
pub struct TotpDisableReq {
    pub password: String,
    pub code: String,
}

pub async fn totp_disable(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<TotpDisableReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "totp", 10).await?;
    let user_id = require_session(&state, &headers).await?;
    let user = identity::reauthenticate(&state.pg, user_id, &req.password)
        .await?
        .ok_or_else(|| AppError::unauthorized("invalid_credentials"))?;
    let sealed = user
        .totp_secret_ciphertext
        .ok_or_else(|| AppError::bad_request().with_param("totp_not_enabled"))?;
    let master = state
        .master_key
        .as_deref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, "totp_disabled"))?;
    let secret = identity::open_totp_secret(master, &sealed).map_err(|_| AppError::internal())?;
    let counter =
        identity::matching_totp_counter(&secret, &req.code, chrono::Utc::now().timestamp())
            .ok_or_else(|| AppError::unauthorized("totp_invalid"))?;
    if !identity::disable_totp(&state.pg, user_id, &sealed, counter).await? {
        return Err(AppError::unauthorized("totp_invalid"));
    }
    Ok(Json(json!({"enabled":false})))
}

// ---- session 兑换 API key（key 单轨的正规入口）----

#[derive(Deserialize)]
pub struct CreateKeyReq {
    /// Lifetime USD budget in micro-USD; absent/null = unlimited.
    #[serde(default)]
    pub quota_micro: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    /// 过期时间（RFC 3339）；缺省 = 永不过期。
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 模型白名单；缺省/空数组 = 不限。新建时填写只可能收窄本 key，无提权面。
    #[serde(default)]
    pub model_allowlist: Option<Vec<String>>,
    /// 分组（须在 /api/me/groups 可选集合内）；缺省 = 跟随用户分组。
    #[serde(default)]
    pub group_code: Option<String>,
    /// IP 白名单（地址 / CIDR；只约束数据面调用）；缺省 = 不限。
    #[serde(default)]
    pub ip_allowlist: Option<Vec<String>>,
}

pub async fn create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<CreateKeyReq>,
) -> Result<Response, AppError> {
    let user_id = require_session(&state, &headers).await?;
    super::super::portal::validate_key_limits(req.quota_micro, req.expires_at)?;
    let token = format!("sk-okapi-{}", rand_token(43));
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let name = match req.name.as_deref() {
        Some(name) => super::super::identifiers::ensure_optional_text("name", name, 128)?,
        None => "web",
    };
    let allowlist = super::super::portal::normalize_allowlist(req.model_allowlist);
    let group_code = req
        .group_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(code) = group_code {
        super::super::portal::ensure_selectable(&state, user_id, code).await?;
    }
    let ip_allowlist = super::super::portal::normalize_ip_allowlist(req.ip_allowlist)?;
    // Missing master key preserves hash-only issuance; never fall back to storing plaintext.
    let ciphertext = state
        .master_key
        .as_deref()
        .map(|master| okapi_store::api_key_secret::seal(master, user_id, &key_hash, &token))
        .transpose()?;
    let key_id = sqlx::query_scalar::<_, i64>(
        r"INSERT INTO api_keys (user_id, key_hash, key_prefix, name, expires_at, model_allowlist, group_override, ip_allowlist, key_ciphertext, quota_mode, quota_micro)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, CASE WHEN $10::bigint IS NULL THEN 0 ELSE 1 END, $10) RETURNING id",
    )
    .bind(user_id)
    .bind(key_hash)
    .bind(token.chars().take(16).collect::<String>())
    .bind(name)
    .bind(req.expires_at)
    .bind(allowlist)
    .bind(group_code)
    .bind(ip_allowlist)
    .bind(&ciphertext)
    .bind(req.quota_micro)
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut response = Json(json!({
        "key_id": key_id,
        // Subsequent reads require an owner web session, not just an API key.
        "api_key": token,
        "copy_available": ciphertext.is_some(),
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store, private".parse().unwrap());
    Ok(response)
}
