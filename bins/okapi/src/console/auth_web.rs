//! /auth/* 自助面（IMPLEMENTATION §6.4）：邮箱密码注册/登录、TOTP 2FA、
//! session 兑换 API key。web session（Redis）只服务本模块；
//! 门户与数据面保持 API key 单轨。
//! Turnstile：settings.turnstile_secret 配置后校验注册 token，未配置跳过（缺省关）。

use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
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

    // 没有真正的 .await：直接给一个就绪的 Future（clippy 1.98 `unused_async_trait_impl`）
    fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|c| c.0),
        )))
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

/// Cookie 头解析会话 id（网关校验登录 key 时也用）。
pub(crate) fn session_id(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == SESSION_COOKIE).then(|| value.to_owned())
    })
}

/// 会话鉴权：Cookie → Redis → user_id。
pub(super) async fn require_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<i64, AppError> {
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
    /// 邮箱验证码（策略 `email_verification` 开启时必填，§11.27）。
    #[serde(default)]
    pub email_code: Option<String>,
}

pub async fn register(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<RegisterReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "register", 5).await?;
    let email = req.email.trim().to_lowercase();
    if !super::identifiers::valid_email(&email) || req.password.len() < 8 {
        return Err(AppError::bad_request().with_param("register_fields"));
    }
    // 与资料页改名同一套规则；此前注册只查非空，超长名在 PG 那里变成 500
    let username = super::identifiers::normalize_username(&req.username)?;
    // 注册策略（§11.16）：关闭 / 邀请制 / 邮箱域名——在 Turnstile 与写库之前判定
    let policy = super::registration::RegistrationPolicy::load(&state).await?;
    super::registration::check(&policy, &email)?;
    let inviter = super::registration::resolve_inviter(&state, req.aff_code.as_deref()).await?;
    if policy.mode == super::registration::RegisterMode::InviteOnly && inviter.is_none() {
        return Err(AppError::new(StatusCode::FORBIDDEN, "invite_required"));
    }
    verify_turnstile(&state, req.turnstile_token.as_deref()).await?;
    // 邮箱验证码（§11.27）：放在 Turnstile 之后、写库之前——码对上即销毁，
    // 若后面 email_taken 用户需重新取码，代价可接受（重复邮箱本就是异常路径）
    if policy.email_verification {
        let code = req
            .email_code
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| AppError::bad_request().with_param("email_code"))?;
        if !state.sched.email_code_take(&email, code).await {
            return Err(AppError::bad_request().with_param("email_code_invalid"));
        }
    }
    // 邮箱冲突在 SQL 里 DO NOTHING；用户名冲突是唯一键报错，此前直接成了 500
    let user_id =
        match identity::register_user(&state.pg, &email, &username, &req.password).await {
            Ok(user_id) => user_id,
            Err(okapi_store::StoreError::Sqlx(sqlx::Error::Database(db)))
                if db.constraint() == Some("users_username_key") =>
            {
                return Err(AppError::new(
                    StatusCode::CONFLICT,
                    "profile_username_taken",
                ));
            }
            Err(err) => return Err(err.into()),
        }
        .ok_or_else(|| AppError::new(StatusCode::CONFLICT, "email_taken"))?;
    bind_inviter(&state, user_id, inviter).await;
    super::registration::grant_credits(&state, &policy, user_id, inviter).await;
    Ok(Json(json!({ "user_id": user_id })))
}

// ---- 邮箱验证码 / 找回密码（IMPLEMENTATION §11.27）----

const EMAIL_CODE_TTL_SECS: i64 = 600;
const EMAIL_CODE_COOLDOWN_SECS: i64 = 60;
const PWRESET_TTL_SECS: i64 = 1800;
/// 每个收件箱每天最多收几封验证码 / 找回密码邮件（两类分开计）。每 IP 限流挡不住
/// 换 IP 轮着发，没有这道闸，任何人都能拿本站给别人的邮箱持续发信。
const MAIL_DAILY_CAP: i64 = 10;

/// 发信限额按收件箱计：去掉 `+标签`，Gmail 再去掉本地部分的点。这些写法投进同一个信箱，
/// 分开计数等于给轰炸者无限个额度。只用于计数，不改用户填写的地址。
fn mailbox_key(email: &str) -> String {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return email.to_owned();
    };
    let local = local.split_once('+').map_or(local, |(base, _)| base);
    if matches!(domain, "gmail.com" | "googlemail.com") {
        return format!("{}@{domain}", local.replace('.', ""));
    }
    format!("{local}@{domain}")
}

/// 收件箱当日发信额度；在判断账号是否存在之前计，限额本身不泄露邮箱是否注册。
async fn mail_daily_guard(state: &AppState, scope: &str, email: &str) -> Result<(), AppError> {
    if state
        .sched
        .mail_daily_incr(scope, &mailbox_key(email))
        .await
        > MAIL_DAILY_CAP
    {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "email_daily_limit",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct EmailCodeReq {
    pub email: String,
    /// 邮件语言（zh-CN / en）；缺省看 Accept-Language。
    #[serde(default)]
    pub lang: Option<String>,
}

fn mail_lang(headers: &HeaderMap, explicit: Option<&str>) -> crate::mail::templates::Lang {
    let accept = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    crate::mail::templates::Lang::resolve(explicit, accept)
}

/// 邮件里的站点名：settings.site_name，缺省 "Okapi"。
async fn site_name(state: &AppState) -> String {
    state
        .setting_cached("site_name")
        .await
        .as_ref()
        .as_ref()
        .and_then(|v| {
            v.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Okapi".to_owned())
}

/// Authentication links must use a configured canonical URL, never request headers.
pub(crate) async fn site_base_url(state: &AppState) -> Result<String, AppError> {
    let setting = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = $1"#, "site_url")
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let base = setting
        .as_ref()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::internal().with_param("site_url_required"))?;
    let url = reqwest::Url::parse(base).map_err(|_| AppError::internal().with_param("site_url"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::internal().with_param("site_url"));
    }
    Ok(base.trim_end_matches('/').to_owned())
}

fn map_mail_error(err: crate::mail::MailError) -> AppError {
    match err {
        crate::mail::MailError::NotConfigured => {
            AppError::new(StatusCode::NOT_IMPLEMENTED, "smtp_not_configured")
        }
        other => {
            tracing::warn!(error = %other, "邮件发送失败");
            AppError::new(StatusCode::BAD_GATEWAY, "smtp_send_failed")
        }
    }
}

/// POST /auth/email-code：注册邮箱验证码。先过注册策略（关闭 / 域名黑白名单都不给码），
/// 每 IP 限流 + 每邮箱 60s 冷却 + 每收件箱每日上限；SMTP 未配置 501。
pub async fn email_code(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<EmailCodeReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "email_code", 3).await?;
    let email = req.email.trim().to_lowercase();
    if !super::identifiers::valid_email(&email) {
        return Err(AppError::bad_request().with_param("email"));
    }
    let policy = super::registration::RegistrationPolicy::load(&state).await?;
    super::registration::check(&policy, &email)?;
    if !policy.email_verification {
        // 策略没开验证却来取码：不是错误，但也没必要发信
        return Err(AppError::bad_request().with_param("email_verification_disabled"));
    }
    let mailer = crate::mail::Mailer::from_state(&state)
        .await
        .map_err(map_mail_error)?;
    if !state
        .sched
        .email_code_cooldown_acquire(&email, EMAIL_CODE_COOLDOWN_SECS)
        .await
    {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "email_code_cooldown",
        ));
    }
    mail_daily_guard(&state, "email_code", &email).await?;
    let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
    if !state
        .sched
        .email_code_set(&email, &code, EMAIL_CODE_TTL_SECS)
        .await
    {
        return Err(AppError::internal());
    }
    let lang = mail_lang(&headers, req.lang.as_deref());
    let site = site_name(&state).await;
    let ttl_min = u32::try_from(EMAIL_CODE_TTL_SECS / 60).unwrap_or(10);
    mailer
        .send(crate::mail::templates::verification_code(
            lang, &site, &email, &code, ttl_min,
        ))
        .await
        .map_err(map_mail_error)?;
    Ok(Json(json!({ "ok": true, "ttl_secs": EMAIL_CODE_TTL_SECS })))
}

#[derive(Deserialize)]
pub struct ForgotReq {
    pub email: String,
    #[serde(default)]
    pub lang: Option<String>,
}

/// POST /auth/password/forgot：无论邮箱是否存在都回 ok（防枚举）；存在且有密码才发信。
/// SMTP 未配置 501——这是配置问题，前端该提示"联系管理员"而不是假装发出去了。
/// 每收件箱每日上限对存在与否一视同仁地计，429 不泄露邮箱是否注册。
pub async fn password_forgot(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<ForgotReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "password_forgot", 3).await?;
    let email = req.email.trim().to_lowercase();
    if !super::identifiers::valid_email(&email) {
        return Err(AppError::bad_request().with_param("email"));
    }
    let mailer = crate::mail::Mailer::from_state(&state)
        .await
        .map_err(map_mail_error)?;
    let base = site_base_url(&state).await?;
    mail_daily_guard(&state, "password_forgot", &email).await?;
    let Some(user_id) = identity::find_password_account(&state.pg, &email).await? else {
        return Ok(Json(json!({ "ok": true })));
    };
    let token = rand_token(32);
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    if !state
        .sched
        .pwreset_set(&token_hash, user_id, PWRESET_TTL_SECS)
        .await
    {
        return Err(AppError::internal());
    }
    let link = format!("{base}/reset-password?token={token}");
    let lang = mail_lang(&headers, req.lang.as_deref());
    let site = site_name(&state).await;
    let ttl_min = u32::try_from(PWRESET_TTL_SECS / 60).unwrap_or(30);
    // 发信失败对外仍 ok：否则"发信失败"与"邮箱不存在"可被区分出来；细节进日志
    if let Err(err) = mailer
        .send(crate::mail::templates::password_reset(
            lang, &site, &email, &link, ttl_min,
        ))
        .await
    {
        tracing::warn!(user_id, error = %err, "找回密码邮件发送失败");
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ResetReq {
    pub token: String,
    pub password: String,
}

/// POST /auth/password/reset：token 一次性；成功后吊销该用户全部 web 会话。
pub async fn password_reset(
    State(state): State<AppState>,
    conn: MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<ResetReq>,
) -> Result<Json<Value>, AppError> {
    critical_rate_guard(&state, &headers, conn.0.as_ref(), "password_reset", 10).await?;
    if req.password.len() < 8 {
        return Err(AppError::bad_request().with_param("password"));
    }
    let token = req.token.trim();
    if token.is_empty() || token.len() > 128 {
        return Err(AppError::bad_request().with_param("token"));
    }
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let Some(user_id) = state.sched.pwreset_take(&token_hash).await else {
        return Err(AppError::bad_request().with_param("reset_token_invalid"));
    };
    if !identity::set_password(&state.pg, user_id, &req.password).await? {
        return Err(AppError::bad_request().with_param("reset_token_invalid"));
    }
    state.sched.web_session_revoke_user(user_id).await;
    Ok(Json(json!({ "ok": true })))
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

const TURNSTILE_VERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

/// Turnstile 校验（settings.turnstile_secret 未配置即跳过）。
/// `settings.turnstile_verify_url` 可覆写 siteverify 地址（内网出口代理 / 自动化用例的 mock），
/// 缺省 Cloudflare 官方端点。
async fn verify_turnstile(state: &AppState, token: Option<&str>) -> Result<(), AppError> {
    let rows = sqlx::query!(
        r#"SELECT key, value #>> '{}' AS "v!" FROM settings
           WHERE key IN ('turnstile_secret', 'turnstile_verify_url')"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut secret = None;
    let mut verify_url = None;
    for row in rows {
        match row.key.as_str() {
            "turnstile_secret" => secret = Some(row.v),
            "turnstile_verify_url" => verify_url = Some(row.v).filter(|v| !v.trim().is_empty()),
            _ => {}
        }
    }
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
    let verify_url = verify_url.unwrap_or_else(|| TURNSTILE_VERIFY_URL.to_owned());
    super::ssrf::validate_url(&state.pg, &verify_url).await?;
    let outcome = state
        .pass
        .probe(okapi_providers::custom_pass::PassRequest {
            method: axum::http::Method::POST,
            url: verify_url,
            auth_header: "x-okapi-noop".to_owned(),
            auth_value: "1".to_owned(),
            content_type: Some("application/x-www-form-urlencoded".to_owned()),
            body: bytes::Bytes::from(body),
            proxy_url: None,
            extra_headers: Vec::new(),
        })
        .await;
    match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { stream, .. }) => {
            let buf = super::outbound_body::collect(stream, 64 * 1024).await?;
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
    ExtractJson(req): ExtractJson<LoginReq>,
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
    super::audit::record_login(
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
pub(super) async fn open_web_session(
    state: &AppState,
    user_id: i64,
    ip: Option<&str>,
    headers: &HeaderMap,
) -> String {
    let sid =
        crate::gateway::sched_redis::SchedulerRedis::web_session_sid(user_id, &rand_token(48));
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
const DEFAULT_WEB_SESSION_LIMIT: i64 = 10;

/// `settings.web_session_limit`：未配置 = [`DEFAULT_WEB_SESSION_LIMIT`]，
/// **显式配 0 仍是"不限"**——保留这个逃生口，但它得是站长主动选的。
pub(super) async fn web_session_limit(state: &AppState) -> i64 {
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
pub(super) fn session_cookie(sid: &str) -> String {
    session_cookie_with_security(sid, secure_cookie())
}

pub(super) fn secure_cookie() -> bool {
    static SECURE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SECURE.get_or_init(|| {
        let secure = okapi_store::env_config::flag("OKAPI_COOKIE_SECURE").unwrap_or(true);
        if !secure {
            tracing::warn!("Secure cookies explicitly disabled for HTTP deployment");
        }
        secure
    })
}

fn session_cookie_with_security(sid: &str, secure: bool) -> String {
    let flag = if secure { "; Secure" } else { "" };
    format!("{SESSION_COOKIE}={sid}{flag}; HttpOnly; SameSite=Lax; Path=/; Max-Age=604800")
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
        let revoked: Vec<String> = sqlx::query_scalar(
            "UPDATE api_keys SET deleted_at = now() \
             WHERE session_hash = $1 AND deleted_at IS NULL RETURNING key_hash",
        )
        .bind(session_hash(&sid))
        .fetch_all(&state.pg)
        .await
        .unwrap_or_default();
        for hash in &revoked {
            state.sched.auth_del(hash).await;
        }
    }
    Json(json!({ "ok": true }))
}

/// 登录 key 记的会话标识：sid 的 sha256 十六进制（sid 本身是能力令牌，不落库）。
fn session_hash(sid: &str) -> String {
    hex::encode(Sha256::digest(sid.as_bytes()))
}

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
    let user_id = state
        .sched
        .web_session_get(&sid)
        .await
        .ok_or_else(unauthorized)?;
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

fn totp_binding(headers: &HeaderMap, user_id: i64, pending: &str) -> Result<String, AppError> {
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
    super::portal::validate_key_limits(req.quota_micro, req.expires_at)?;
    let token = format!("sk-okapi-{}", rand_token(43));
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let name = match req.name.as_deref() {
        Some(name) => super::identifiers::ensure_optional_text("name", name, 128)?,
        None => "web",
    };
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
    Ok(Json(json!({ "ok": true })))
}

/// Public revocation handle; never usable as a login cookie.
fn session_fingerprint(sid: &str) -> String {
    hex::encode(Sha256::digest(sid.as_bytes()))[..32].to_owned()
}

/// DELETE /api/me/sessions：吊销该用户全部 web 会话。
pub async fn revoke_all_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    state.sched.web_session_revoke_user(key.user_id).await;
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

#[cfg(test)]
mod mailbox_tests {
    use super::mailbox_key;

    #[test]
    fn variants_of_one_inbox_share_a_quota() {
        assert_eq!(mailbox_key("a.b+x@ok.test"), "a.b@ok.test");
        assert_eq!(mailbox_key("a.b+x+y@gmail.com"), "ab@gmail.com");
        assert_eq!(mailbox_key("a.b@googlemail.com"), "ab@googlemail.com");
        assert_eq!(mailbox_key("plain@ok.test"), "plain@ok.test");
        assert_eq!(mailbox_key("no-at-sign"), "no-at-sign");
    }
}
