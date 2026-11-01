//! /auth/* 自助面（IMPLEMENTATION §6.4）：邮箱密码注册/登录、TOTP 2FA、
//! session 兑换 API key。web session（Redis）只服务本模块；
//! 门户与数据面保持 API key 单轨。
//! Turnstile：settings.turnstile_secret 配置后校验注册 token，未配置跳过（缺省关）。
//!
//! 本文件放会话 cookie 与会话读取、敏感端点限速等共用部分；注册 / 验证码 / 密码在 account，
//! 登录与会话管理在 sessions，会话 key / TOTP / 自建 key 在 credentials。

use axum::http::{HeaderMap, StatusCode};
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde_json::Value;
use sha2::{Digest, Sha256};

mod account;
mod credentials;
mod sessions;

pub use self::account::{
    EmailCodeReq, ForgotReq, RegisterReq, ResetReq, email_code, password_forgot, password_reset,
    register,
};
pub use self::credentials::{
    CreateKeyReq, SessionKeyReq, TotpConfirmReq, TotpDisableReq, TotpPasswordReq, create_key,
    session_key, totp_confirm, totp_disable, totp_enroll,
};
pub use self::sessions::{
    LoginReq, list_sessions, login, logout, revoke_all_sessions, revoke_session,
};
pub(super) use self::sessions::{open_web_session, secure_cookie, session_cookie};

use okapi_kernel::auth::{SESSION_COOKIE, session_id};

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
    let ip = okapi_kernel::clients::detect_client_ip(headers)
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

/// 会话鉴权：Cookie → Redis → user_id。
pub(super) async fn require_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<i64, AppError> {
    let sid = session_id(headers)
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))?;
    session_user(state, &sid).await
}

/// 会话 id → 用户。会话存储不可达是服务端故障：报 503，不伪装成 401——前端把 401 当成
/// 「登录失效」直接踢回登录页，Redis 抖一下全站用户都被登出；数据面鉴权同样把两者分开。
pub(super) async fn session_user(state: &AppState, sid: &str) -> Result<i64, AppError> {
    session_outcome(state.sched.web_session_check(sid).await)
}

fn session_outcome(checked: Result<Option<i64>, fred::error::Error>) -> Result<i64, AppError> {
    match checked {
        Ok(Some(user_id)) => Ok(user_id),
        Ok(None) => Err(AppError::unauthorized(okapi_api::codes::INVALID_API_KEY)),
        Err(error) => {
            tracing::error!(%error, "web session store unreachable");
            Err(AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                okapi_api::codes::AUTH_BACKEND_UNAVAILABLE,
            ))
        }
    }
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

/// 登录 key 记的会话标识：sid 的 sha256 十六进制（sid 本身是能力令牌，不落库）。
fn session_hash(sid: &str) -> String {
    hex::encode(Sha256::digest(sid.as_bytes()))
}

#[cfg(test)]
mod session_outcome_tests {
    use super::session_outcome;
    use axum::http::StatusCode;

    /// 会话不存在是 401，会话存储不可达是 503：前端把 401 当「登录失效」踢回登录页，
    /// Redis 抖一下不能让全站用户都被登出。
    #[test]
    fn a_dead_session_is_401_and_an_unreachable_store_is_503() {
        assert_eq!(session_outcome(Ok(Some(7))).unwrap(), 7);
        let gone = session_outcome(Ok(None)).unwrap_err();
        assert_eq!(gone.status, StatusCode::UNAUTHORIZED);
        let down = session_outcome(Err(fred::error::Error::new(
            fred::error::ErrorKind::IO,
            "connection refused",
        )))
        .unwrap_err();
        assert_eq!(down.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(down.code, okapi_api::codes::AUTH_BACKEND_UNAVAILABLE);
    }
}
