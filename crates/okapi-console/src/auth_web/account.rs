//! 账号：注册、邮箱验证码、找回 / 重置密码、邀请人绑定与 Turnstile 校验。
use super::{MaybeConnectInfo, critical_rate_guard, rand_token, site_base_url, site_name};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_store::identity;
use rand::RngExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

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
    if !super::super::identifiers::valid_email(&email) || req.password.len() < 8 {
        return Err(AppError::bad_request().with_param("register_fields"));
    }
    // 与资料页改名同一套规则；此前注册只查非空，超长名在 PG 那里变成 500
    let username = super::super::identifiers::normalize_username(&req.username)?;
    // 注册策略（§11.16）：关闭 / 邀请制 / 邮箱域名——在 Turnstile 与写库之前判定
    let policy = super::super::registration::RegistrationPolicy::load(&state).await?;
    super::super::registration::check(&policy, &email)?;
    let inviter =
        super::super::registration::resolve_inviter(&state, req.aff_code.as_deref()).await?;
    if policy.mode == super::super::registration::RegisterMode::InviteOnly && inviter.is_none() {
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
    super::super::registration::grant_credits(&state, &policy, user_id, inviter).await;
    Ok(Json(json!({ "user_id": user_id })))
}

// ---- 邮箱验证码 / 找回密码（IMPLEMENTATION §11.27）----

pub(super) const EMAIL_CODE_TTL_SECS: i64 = 600;
pub(super) const EMAIL_CODE_COOLDOWN_SECS: i64 = 60;
pub(super) const PWRESET_TTL_SECS: i64 = 1800;
/// 每个收件箱每天最多收几封验证码 / 找回密码邮件（两类分开计）。每 IP 限流挡不住
/// 换 IP 轮着发，没有这道闸，任何人都能拿本站给别人的邮箱持续发信。
pub(super) const MAIL_DAILY_CAP: i64 = 10;

/// 发信限额按收件箱计：去掉 `+标签`，Gmail 再去掉本地部分的点。这些写法投进同一个信箱，
/// 分开计数等于给轰炸者无限个额度。只用于计数，不改用户填写的地址。
pub(super) fn mailbox_key(email: &str) -> String {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return email.to_owned();
    };
    let local = local.split_once('+').map_or(local, |(base, _)| base);
    // googlemail.com 是 gmail.com 的别名，两个域名投进同一个信箱
    if matches!(domain, "gmail.com" | "googlemail.com") {
        return format!("{}@gmail.com", local.replace('.', ""));
    }
    format!("{local}@{domain}")
}

/// 收件箱当日发信额度；在判断账号是否存在之前计，限额本身不泄露邮箱是否注册。
pub(super) async fn mail_daily_guard(
    state: &AppState,
    scope: &str,
    email: &str,
) -> Result<(), AppError> {
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

pub(super) fn mail_lang(
    headers: &HeaderMap,
    explicit: Option<&str>,
) -> okapi_kernel::mail::templates::Lang {
    let accept = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    okapi_kernel::mail::templates::Lang::resolve(explicit, accept)
}

pub(super) fn map_mail_error(err: okapi_kernel::mail::MailError) -> AppError {
    match err {
        okapi_kernel::mail::MailError::NotConfigured => {
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
    if !super::super::identifiers::valid_email(&email) {
        return Err(AppError::bad_request().with_param("email"));
    }
    let policy = super::super::registration::RegistrationPolicy::load(&state).await?;
    super::super::registration::check(&policy, &email)?;
    if !policy.email_verification {
        // 策略没开验证却来取码：不是错误，但也没必要发信
        return Err(AppError::bad_request().with_param("email_verification_disabled"));
    }
    let mailer = okapi_kernel::mail::Mailer::from_state(&state)
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
        .send(okapi_kernel::mail::templates::verification_code(
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
    if !super::super::identifiers::valid_email(&email) {
        return Err(AppError::bad_request().with_param("email"));
    }
    let mailer = okapi_kernel::mail::Mailer::from_state(&state)
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
        .send(okapi_kernel::mail::templates::password_reset(
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
    // 改了密码、踢掉了全部会话：和门户里吊销会话一样留审计（这里没有 key，凭的是重置 token）
    if let Err(error) = okapi_store::admin::record_audit(
        &state.pg,
        &format!("user:{user_id}"),
        "user.password_reset",
        &user_id.to_string(),
        json!({"sessions_revoked": true}),
    )
    .await
    {
        tracing::error!(%error, "找回密码审计写入失败");
    }
    Ok(Json(json!({ "ok": true })))
}

/// aff 邀请绑定（M4）：邀请人已在策略层解析；自邀不可能（新用户还没有 aff 码）。
/// 绑定失败不阻注册——邀请关系是增益信息。
pub(super) async fn bind_inviter(state: &AppState, user_id: i64, inviter: Option<i64>) {
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

pub(super) const TURNSTILE_VERIFY_URL: &str =
    "https://challenges.cloudflare.com/turnstile/v0/siteverify";

/// Turnstile 校验（settings.turnstile_secret 未配置即跳过）。
/// `settings.turnstile_verify_url` 可覆写 siteverify 地址（内网出口代理 / 自动化用例的 mock），
/// 缺省 Cloudflare 官方端点。
pub(super) async fn verify_turnstile(
    state: &AppState,
    token: Option<&str>,
) -> Result<(), AppError> {
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
    okapi_kernel::ssrf::validate_url(&state.pg, &verify_url).await?;
    let outcome = state
        .upstreams
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
            let buf = super::super::outbound_body::collect(stream, 64 * 1024).await?;
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

pub(super) fn urlencoding_escape(s: &str) -> String {
    // 表单值最小转义（secret/token 均为 URL-safe 字符集，防御性处理 & = %）
    s.replace('%', "%25")
        .replace('&', "%26")
        .replace('=', "%3D")
}

#[cfg(test)]
mod mailbox_tests {
    use super::mailbox_key;

    #[test]
    fn variants_of_one_inbox_share_a_quota() {
        assert_eq!(mailbox_key("a.b+x@ok.test"), "a.b@ok.test");
        assert_eq!(mailbox_key("a.b+x+y@gmail.com"), "ab@gmail.com");
        // googlemail.com 是 gmail.com 的别名：同一个信箱不能靠换域名拿到第二份额度
        assert_eq!(mailbox_key("a.b@googlemail.com"), "ab@gmail.com");
        assert_eq!(
            mailbox_key("a.b@googlemail.com"),
            mailbox_key("ab+tag@gmail.com")
        );
        assert_eq!(mailbox_key("plain@ok.test"), "plain@ok.test");
        assert_eq!(mailbox_key("no-at-sign"), "no-at-sign");
    }
}
