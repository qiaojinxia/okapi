//! 通用 OAuth2/OIDC 登录（IMPLEMENTATION §6.4）：配置驱动 authorization-code 流。
//! settings.oauth_providers = [{code, client_id, client_secret, authorize_url?, token_url?,
//! userinfo_url?, scopes?, subject_field?, display_field?}]；
//! github/discord/linuxdo 有内置预设（只需 client_id/secret）。
//! state 走 Redis 一次性键（10min）；首登自动注册 + 绑定 (provider, subject)，
//! 成功后发 web session（前端经 /auth/keys 兑 key，保持 key 单轨）。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okapi_providers::custom_pass::{PassRequest, PassResponse};
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::Value;

const STATE_TTL_SECS: i64 = 600;

#[derive(Clone, Deserialize)]
struct ProviderCfg {
    code: String,
    client_id: String,
    client_secret: String,
    #[serde(default)]
    authorize_url: Option<String>,
    #[serde(default)]
    token_url: Option<String>,
    #[serde(default)]
    userinfo_url: Option<String>,
    #[serde(default)]
    scopes: Option<String>,
    #[serde(default)]
    subject_field: Option<String>,
    #[serde(default)]
    display_field: Option<String>,
}

/// 内置预设（authorize/token/userinfo/scopes/字段名）。
fn preset(
    code: &str,
) -> Option<(
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
)> {
    match code {
        "github" => Some((
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            "https://api.github.com/user",
            "read:user",
            "id",
            "login",
        )),
        "discord" => Some((
            "https://discord.com/oauth2/authorize",
            "https://discord.com/api/oauth2/token",
            "https://discord.com/api/users/@me",
            "identify",
            "id",
            "username",
        )),
        "linuxdo" => Some((
            "https://connect.linux.do/oauth2/authorize",
            "https://connect.linux.do/oauth2/token",
            "https://connect.linux.do/api/user",
            "read",
            "id",
            "username",
        )),
        _ => None,
    }
}

struct ResolvedProvider {
    code: String,
    client_id: String,
    client_secret: String,
    authorize_url: String,
    token_url: String,
    userinfo_url: String,
    scopes: String,
    subject_field: String,
    display_field: String,
}

async fn load_provider(state: &AppState, code: &str) -> Result<ResolvedProvider, AppError> {
    let raw = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'oauth_providers'"#)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "oauth_provider_not_configured"))?;
    let list: Vec<ProviderCfg> = serde_json::from_value(raw).map_err(|_| AppError::internal())?;
    let cfg = list
        .into_iter()
        .find(|p| p.code == code)
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "oauth_provider_not_configured"))?;
    let fallback = preset(code);
    let pick = |explicit: Option<String>, idx: usize| -> Result<String, AppError> {
        explicit
            .or_else(|| fallback.map(|f| [f.0, f.1, f.2, f.3, f.4, f.5][idx].to_owned()))
            .ok_or_else(|| AppError::bad_request().with_param("oauth_provider_urls"))
    };
    Ok(ResolvedProvider {
        code: cfg.code.clone(),
        client_id: cfg.client_id,
        client_secret: cfg.client_secret,
        authorize_url: pick(cfg.authorize_url, 0)?,
        token_url: pick(cfg.token_url, 1)?,
        userinfo_url: pick(cfg.userinfo_url, 2)?,
        scopes: pick(cfg.scopes, 3)?,
        subject_field: pick(cfg.subject_field, 4)?,
        display_field: pick(cfg.display_field, 5)?,
    })
}

/// 回调地址：settings.site_url 优先，缺省从请求 Host 推导。
async fn redirect_uri(state: &AppState, headers: &HeaderMap, code: &str) -> String {
    let base = sqlx::query_scalar!(
        r#"SELECT value #>> '{}' AS "v!" FROM settings WHERE key = 'site_url'"#
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");
        format!("http://{host}")
    });
    format!("{}/auth/oauth/{code}/callback", base.trim_end_matches('/'))
}

fn form_escape(s: &str) -> String {
    s.replace('%', "%25")
        .replace('&', "%26")
        .replace('=', "%3D")
}

/// GET /auth/oauth/{provider}：302 到 IdP 授权页。
pub async fn start(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let provider = load_provider(&state, &code).await?;
    let token: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    state.sched.oauth_state_set(&token, STATE_TTL_SECS).await;
    let redirect = redirect_uri(&state, &headers, &provider.code).await;
    let location = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={token}",
        provider.authorize_url,
        form_escape(&provider.client_id),
        form_escape(&redirect),
        form_escape(&provider.scopes),
    );
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
}

/// GET /auth/oauth/{provider}/callback：state 校验 → 换 token → userinfo →
/// 绑定/注册 → session cookie → 302 前端。
pub async fn callback(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    if !state.sched.oauth_state_take(&q.state).await {
        return Err(AppError::unauthorized("oauth_state_invalid"));
    }
    let provider = load_provider(&state, &code).await?;
    let redirect = redirect_uri(&state, &headers, &provider.code).await;

    // authorization code → access_token
    let body = format!(
        "grant_type=authorization_code&code={}&client_id={}&client_secret={}&redirect_uri={}",
        form_escape(&q.code),
        form_escape(&provider.client_id),
        form_escape(&provider.client_secret),
        form_escape(&redirect),
    );
    let token_resp = fetch_json(
        &state,
        PassRequest {
            method: axum::http::Method::POST,
            url: provider.token_url.clone(),
            auth_header: "accept".to_owned(),
            auth_value: "application/json".to_owned(),
            content_type: Some("application/x-www-form-urlencoded".to_owned()),
            body: bytes::Bytes::from(body),
        },
    )
    .await?;
    let access_token = token_resp
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::unauthorized("oauth_token_exchange_failed"))?
        .to_owned();

    // userinfo
    let userinfo = fetch_json(
        &state,
        PassRequest {
            method: axum::http::Method::GET,
            url: provider.userinfo_url.clone(),
            auth_header: "authorization".to_owned(),
            auth_value: format!("Bearer {access_token}"),
            content_type: None,
            body: bytes::Bytes::new(),
        },
    )
    .await?;
    let subject = userinfo
        .get(&provider.subject_field)
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .filter(|s| !s.is_empty() && s != "null")
        .ok_or_else(|| AppError::unauthorized("oauth_userinfo_missing_subject"))?;
    let display = userinfo
        .get(&provider.display_field)
        .and_then(Value::as_str)
        .unwrap_or(&subject)
        .to_owned();

    let user_id =
        okapi_store::identity::link_oauth_user(&state.pg, &provider.code, &subject, &display)
            .await?;

    // web session + 回前端（?oauth=done 触发兑 key）
    let sid: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect();
    state.sched.web_session_set(&sid, user_id).await;
    let cookie = format!("okapi_session={sid}; HttpOnly; SameSite=Lax; Path=/; Max-Age=604800");
    let mut resp = (
        StatusCode::FOUND,
        [(header::LOCATION, "/?oauth=done".to_owned())],
    )
        .into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(resp)
}

/// 经 PassUpstream 请求并解析 JSON（token/userinfo 共用）。
async fn fetch_json(state: &AppState, req: PassRequest) -> Result<Value, AppError> {
    match state.pass.forward(req).await {
        Ok(PassResponse::Ok { mut stream, .. }) => {
            use futures::StreamExt as _;
            let mut buf = Vec::new();
            while let Some(Ok(chunk)) = stream.next().await {
                buf.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&buf)
                .map_err(|_| AppError::unauthorized("oauth_upstream_not_json"))
        }
        Ok(PassResponse::ErrStatus { status, .. }) => {
            Err(AppError::unauthorized("oauth_upstream_error")
                .with_param(format!("status_{status}")))
        }
        Err(_) => Err(AppError::unauthorized("oauth_upstream_unreachable")),
    }
}

/// GET /auth/oauth-providers：可用登录方式（仅 code 列表，公开）。
pub async fn list_providers(State(state): State<AppState>) -> Result<axum::Json<Value>, AppError> {
    let raw = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'oauth_providers'"#)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let codes: Vec<String> = raw
        .and_then(|v| serde_json::from_value::<Vec<ProviderCfg>>(v).ok())
        .map(|list| list.into_iter().map(|p| p.code).collect())
        .unwrap_or_default();
    Ok(axum::Json(serde_json::json!({ "providers": codes })))
}
