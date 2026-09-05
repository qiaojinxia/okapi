//! /auth/* 自助面（IMPLEMENTATION §6.4）：邮箱密码注册/登录、TOTP 2FA、
//! session 兑换 API key。web session（Redis）只服务本模块；
//! 门户与数据面保持 API key 单轨。
//! Turnstile：settings.turnstile_secret 配置后校验注册 token，未配置跳过（缺省关）。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okapi_store::identity;
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SESSION_COOKIE: &str = "okapi_session";

/// 直连 socket 地址的可缺省提取器（serve 未挂 with_connect_info 时为 None，
/// 如集成测试的裸 serve；生产 console 已挂，见 mod.rs run）。
pub struct MaybeConnectInfo(pub Option<std::net::SocketAddr>);

impl<S> axum::extract::FromRequestParts<S> for MaybeConnectInfo
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// 关键接口每 IP 限流（对齐 new-api rc.24）：60s 固定窗。
/// 覆盖 login/register/totp/redeem 四个爆破面；配额走
/// `settings.critical_rate_limits`（对象，键=scope，0=关闭），缺省见调用点。
/// IP 取 CDN 头，缺省回退直连 socket；两者皆无（纯测试环境）放行。
pub async fn critical_rate_guard(
    state: &AppState,
    headers: &HeaderMap,
    conn: Option<&std::net::SocketAddr>,
    scope: &str,
    default_per_min: i64,
) -> Result<(), AppError> {
    let limit = state
        .setting_cached("critical_rate_limits")
        .await
        .as_ref()
        .as_ref()
        .and_then(|v| v.get(scope))
        .and_then(Value::as_i64)
        .unwrap_or(default_per_min);
    if limit <= 0 {
        return Ok(());
    }
    let ip = crate::gateway::clients::detect_client_ip(headers)
        .or_else(|| conn.map(|a| a.ip().to_string()));
    let Some(ip) = ip else {
        return Ok(());
    };
    let count = state.sched.crit_rate_incr(scope, &ip).await;
    if count > limit {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            okapi_api::codes::RATE_LIMITED,
        )
        .with_param(scope));
    }
    Ok(())
}

fn rand_token(len: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

/// Cookie 头解析会话 id。
fn session_id(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == SESSION_COOKIE).then(|| value.to_owned())
    })
}

/// 会话鉴权：Cookie → Redis → user_id。
async fn require_session(state: &AppState, headers: &HeaderMap) -> Result<i64, AppError> {
    let sid = session_id(headers)
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))?;
    state
        .sched
        .web_session_get(&sid)
        .await
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))
}

// ---- 注册 ----

#[derive(Deserialize)]
pub struct RegisterReq {
    pub email: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub turnstile_token: Option<String>,
    /// 邀请码（可选；无效/自邀静默忽略，不阻注册）。
    #[serde(default)]
    pub aff_code: Option<String>,
}

pub async fn register(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    Json(req): Json<RegisterReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "register", 5).await?;
    let email = req.email.trim().to_lowercase();
    if !email.contains('@') || req.username.trim().is_empty() || req.password.len() < 8 {
        return Err(AppError::bad_request().with_param("register_fields"));
    }
    // 注册策略（§11.16）：关闭 / 邀请制 / 邮箱域名——在 Turnstile 与写库之前判定
    let policy = super::registration::RegistrationPolicy::load(&state).await;
    super::registration::check(&policy, &email)?;
    let inviter = super::registration::resolve_inviter(&state, req.aff_code.as_deref()).await?;
    if policy.mode == super::registration::RegisterMode::InviteOnly && inviter.is_none() {
        return Err(AppError::new(StatusCode::FORBIDDEN, "invite_required"));
    }
    verify_turnstile(&state, req.turnstile_token.as_deref()).await?;
    let user_id = identity::register_user(&state.pg, &email, req.username.trim(), &req.password)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::CONFLICT, "email_taken"))?;
    bind_inviter(&state, user_id, inviter).await;
    super::registration::grant_credits(&state, &policy, user_id, inviter).await;
    Ok(Json(json!({ "user_id": user_id })))
}

/// aff 邀请绑定（M4）：邀请人已在策略层解析；自邀不可能（新用户还没有 aff 码）。
/// 绑定失败不阻注册——邀请关系是增益信息。
async fn bind_inviter(state: &AppState, user_id: i64, inviter: Option<i64>) {
    let Some(inviter_id) = inviter else {
        return;
    };
    let result = sqlx::query!(
        r#"UPDATE users SET inviter_id = $2, updated_at = now() WHERE id = $1 AND $2 <> $1"#,
        user_id,
        inviter_id
    )
    .execute(&state.pg)
    .await;
    if let Err(err) = result {
        tracing::warn!(user_id, error = %err, "aff 绑定失败（忽略）");
    }
}

/// Turnstile 校验（settings.turnstile_secret 未配置即跳过）。
async fn verify_turnstile(state: &AppState, token: Option<&str>) -> Result<(), AppError> {
    let secret = sqlx::query_scalar!(
        r#"SELECT value #>> '{}' AS "v!" FROM settings WHERE key = 'turnstile_secret'"#
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let Some(secret) = secret else {
        return Ok(());
    };
    let Some(token) = token else {
        return Err(AppError::bad_request().with_param("turnstile_token"));
    };
    let body = format!(
        "secret={}&response={}",
        urlencoding_escape(&secret),
        urlencoding_escape(token)
    );
    let outcome = state
        .pass
        .forward(okapi_providers::custom_pass::PassRequest {
            method: axum::http::Method::POST,
            url: "https://challenges.cloudflare.com/turnstile/v0/siteverify".to_owned(),
            auth_header: "x-okapi-noop".to_owned(),
            auth_value: "1".to_owned(),
            content_type: Some("application/x-www-form-urlencoded".to_owned()),
            body: bytes::Bytes::from(body),
        })
        .await;
    match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { mut stream, .. }) => {
            use futures::StreamExt as _;
            let mut buf = Vec::new();
            while let Some(Ok(chunk)) = stream.next().await {
                buf.extend_from_slice(&chunk);
            }
            let ok = serde_json::from_slice::<Value>(&buf)
                .ok()
                .and_then(|v| v.get("success").and_then(Value::as_bool))
                .unwrap_or(false);
            if ok {
                Ok(())
            } else {
                Err(AppError::bad_request().with_param("turnstile_failed"))
            }
        }
        _ => Err(AppError::bad_request().with_param("turnstile_unreachable")),
    }
}

fn urlencoding_escape(s: &str) -> String {
    // 表单值最小转义（secret/token 均为 URL-safe 字符集，防御性处理 & = %）
    s.replace('%', "%25")
        .replace('&', "%26")
        .replace('=', "%3D")
}

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
    Json(req): Json<LoginReq>,
) -> Result<Response, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "login", 10).await?;
    let email = req.email.trim().to_lowercase();
    let ip = crate::gateway::clients::detect_client_ip(&headers)
        .or_else(|| conn.0.as_ref().map(|a| a.ip().to_string()));

    // 凭证与 TOTP 校验单独成段：成功与失败都要落审计（§3.5 登录 = 审计 user.login），
    // 失败原因只进审计，对客户端仍是同一个 401
    let verified = verify_login(&state, &email, &req).await;
    let user = match verified {
        Ok(user) => user,
        Err((reason, err)) => {
            super::audit::record_login(&state, &email, None, false, Some(reason), ip, &headers)
                .await;
            return Err(err);
        }
    };
    super::audit::record_login(&state, &email, Some(user.user_id), true, None, ip, &headers).await;

    let sid = rand_token(48);
    state.sched.web_session_set(&sid, user.user_id).await;
    let mut resp = Json(json!({ "user_id": user.user_id, "role": user.role })).into_response();
    let cookie = format!("{SESSION_COOKIE}={sid}; HttpOnly; SameSite=Lax; Path=/; Max-Age=604800");
    if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(resp)
}

/// 密码 + TOTP 校验；`Err((审计原因, 对外错误))`。
async fn verify_login(
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
        if !identity::verify_totp(&secret, code, chrono::Utc::now().timestamp()) {
            return Err(("totp_invalid", AppError::unauthorized("totp_invalid")));
        }
    }
    Ok(user)
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    if let Some(sid) = session_id(&headers) {
        state.sched.web_session_del(&sid).await;
    }
    Json(json!({ "ok": true }))
}

// ---- TOTP 两段式注册 ----

pub async fn totp_enroll(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user_id = require_session(&state, &headers).await?;
    let master = state
        .master_key
        .as_deref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, "totp_disabled"))?;
    let (secret, otpauth_url) = identity::generate_totp_secret(&user_id.to_string());
    let sealed = identity::seal_totp_secret(master, &secret).map_err(|_| AppError::internal())?;
    Ok(Json(json!({
        "otpauth_url": otpauth_url,
        // 服务端密文回执（客户端持有无妨，密钥在服务端；confirm 时带回）
        "pending": hex::encode(sealed),
    })))
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
    Json(req): Json<TotpConfirmReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "totp", 10).await?;
    let user_id = require_session(&state, &headers).await?;
    let master = state
        .master_key
        .as_deref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, "totp_disabled"))?;
    let sealed = hex::decode(&req.pending).map_err(|_| AppError::bad_request())?;
    let secret =
        identity::open_totp_secret(master, &sealed).map_err(|_| AppError::bad_request())?;
    if !identity::verify_totp(&secret, &req.code, chrono::Utc::now().timestamp()) {
        return Err(AppError::bad_request().with_param("totp_code"));
    }
    identity::enable_totp(&state.pg, user_id, &sealed).await?;
    Ok(Json(json!({ "enabled": true })))
}

// ---- session 兑换 API key（key 单轨的正规入口）----

#[derive(Deserialize)]
pub struct CreateKeyReq {
    #[serde(default)]
    pub name: Option<String>,
    /// 过期时间（RFC 3339）；缺省 = 永不过期。
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 模型白名单；缺省/空数组 = 不限。新建时填写只可能收窄本 key，无提权面。
    #[serde(default)]
    pub model_allowlist: Option<Vec<String>>,
    /// 档位（须在 /api/me/groups 可选集合内）；缺省 = 跟随用户分组。
    #[serde(default)]
    pub group_code: Option<String>,
    /// IP 白名单（地址 / CIDR；只约束数据面调用）；缺省 = 不限。
    #[serde(default)]
    pub ip_allowlist: Option<Vec<String>>,
}

pub async fn create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateKeyReq>,
) -> Result<Json<Value>, AppError> {
    let user_id = require_session(&state, &headers).await?;
    let token = format!("sk-okapi-{}", rand_token(43));
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let name = req.name.as_deref().map_or("web", str::trim);
    let allowlist = super::portal::normalize_allowlist(req.model_allowlist);
    let group_code = req
        .group_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(code) = group_code {
        super::portal::ensure_selectable(&state, user_id, code).await?;
    }
    let ip_allowlist = super::portal::normalize_ip_allowlist(req.ip_allowlist)?;
    let key_id = sqlx::query_scalar!(
        r#"INSERT INTO api_keys (user_id, key_hash, key_prefix, name, expires_at, model_allowlist, group_override, ip_allowlist)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id"#,
        user_id,
        key_hash,
        token.chars().take(16).collect::<String>(),
        name,
        req.expires_at,
        allowlist,
        group_code,
        ip_allowlist
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({
        "key_id": key_id,
        // 明文仅本次返回
        "api_key": token,
    })))
}
