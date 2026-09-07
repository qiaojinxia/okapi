//! 渠道 OAuth 登录流程（IMPLEMENTATION §11.38）：站长把自己的 Claude Pro/Max / ChatGPT 订阅登录
//! 成一条 `anthropic_max` / `codex` 渠道。
//!
//! 两步：`start` 生成 PKCE 并把 verifier 存 Redis（10min，一次性），回授权 URL；站长在浏览器里
//! 登录，把回调页 / 地址栏里的 code 贴回 `exchange`，这里换 token → 建渠道或给既有渠道加一把 key。
//! 不监听本地回调端口：网关多半跑在服务器上、浏览器在站长电脑上，贴回 code 对所有形态都成立。

use super::admin::{audit, ensure_channel_owner, guard_scoped};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_providers::UpstreamError;
use okapi_providers::oauth::{Pkce, Tokens, anthropic_max, codex};
use okapi_store::credential::OAuthCredential;
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::{Value, json};

const STATE_TTL_SECS: i64 = 600;

/// 支持登录的两家（其它 provider 400 `provider`）。
fn is_oauth_provider(provider: &str) -> bool {
    matches!(provider, "anthropic_max" | "codex")
}

#[derive(Deserialize)]
pub struct StartReq {
    pub provider: String,
}

/// POST /admin/channels/oauth/start → `{authorize_url, state, redirect_hint}`。
pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StartReq>,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    if !is_oauth_provider(&req.provider) {
        return Err(AppError::bad_request().with_param("provider"));
    }
    let pkce = Pkce::generate();
    // Anthropic 的流程把 verifier 当 state 原样带回；Codex 用独立随机 state
    let oauth_state: String = if req.provider == "anthropic_max" {
        pkce.verifier.clone()
    } else {
        rand::rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect()
    };
    let authorize_url = match req.provider.as_str() {
        "anthropic_max" => anthropic_max::authorize_url(&pkce),
        _ => codex::authorize_url(&pkce, &oauth_state),
    };
    let stored = json!({ "provider": req.provider, "verifier": pkce.verifier });
    if !state
        .sched
        .oauth_cred_state_set(&oauth_state, &stored.to_string(), STATE_TTL_SECS)
        .await
    {
        return Err(AppError::internal());
    }
    Ok(Json(json!({
        "provider": req.provider,
        "authorize_url": authorize_url,
        "state": oauth_state,
        "redirect_uri": match req.provider.as_str() {
            "anthropic_max" => anthropic_max::REDIRECT_URI,
            _ => codex::REDIRECT_URI,
        },
    })))
}

#[derive(Deserialize)]
pub struct ExchangeReq {
    pub state: String,
    /// 回调页显示的 code（Anthropic 为 `code#state`）或整个回调 URL（Codex）。
    pub code: String,
    /// 新建渠道名（`channel_id` 为空时必填）。
    #[serde(default)]
    pub name: Option<String>,
    /// 新建渠道服务的模型名（`channel_id` 为空时必填、非空）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 给既有渠道追加一把 key（同 provider）。
    #[serde(default)]
    pub channel_id: Option<i64>,
    /// token 端点覆写（测试 mock / 企业代理）；落 `settings.oauth_token_url`。
    #[serde(default)]
    pub token_url: Option<String>,
}

/// POST /admin/channels/oauth/exchange → 换 token、建渠道 / 加 key。
// 换码 → 凭证 → 建渠道 / 加 key 的线性时序放同一视野
#[allow(clippy::too_many_lines)]
pub async fn exchange(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ExchangeReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let stored = state
        .sched
        .oauth_cred_state_take(&req.state)
        .await
        .ok_or_else(|| AppError::new(StatusCode::BAD_REQUEST, "oauth_state_invalid"))?;
    let stored: Value = serde_json::from_str(&stored).map_err(|_| AppError::internal())?;
    let provider = stored["provider"].as_str().unwrap_or_default().to_owned();
    let verifier = stored["verifier"].as_str().unwrap_or_default().to_owned();
    if !is_oauth_provider(&provider) {
        return Err(AppError::bad_request().with_param("provider"));
    }
    let token_url = req
        .token_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(url) = token_url {
        super::ssrf::validate_api_base(&state, url).await?;
    }
    // 追加到既有渠道：属主范围与 provider 一致性在换码**之前**判——授权码一次性，
    // 被拒的请求不该先把它烧掉；own 范围的渠道管理员也不能往别人的渠道里塞 key
    if let Some(channel_id) = req.channel_id {
        ensure_channel_owner(&state, channel_id, &actor, scope).await?;
        let existing = sqlx::query_scalar!(
            r#"SELECT provider FROM channels WHERE id = $1 AND deleted_at IS NULL"#,
            channel_id
        )
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
        if existing != provider {
            return Err(AppError::bad_request().with_param("channel_provider_mismatch"));
        }
    }

    let http = state.upstream.http();
    let tokens: Tokens = if provider == "anthropic_max" {
        let (code, _state) = anthropic_max::split_pasted_code(&req.code);
        anthropic_max::exchange(
            http,
            token_url.unwrap_or(anthropic_max::TOKEN_URL),
            code,
            &verifier,
        )
        .await
    } else {
        let (code, _state) = codex::split_pasted_code(&req.code);
        codex::exchange(
            http,
            token_url.unwrap_or(codex::TOKEN_URL),
            &code,
            &verifier,
        )
        .await
    }
    .map_err(|err| match err {
        UpstreamError::Status { status, .. } => {
            AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                .with_param(format!("oauth_exchange_status_{status}"))
        }
        other => AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
            .with_param(other.error_code()),
    })?;
    let Some(refresh_token) = tokens.refresh_token.clone() else {
        // 没有 refresh token 的登录撑不过一次到期，不落库
        return Err(AppError::bad_request().with_param("oauth_no_refresh_token"));
    };
    let now = chrono::Utc::now().timestamp();
    let cred = OAuthCredential {
        access_token: tokens.access_token,
        refresh_token,
        expires_at: now + tokens.expires_in,
        account_id: tokens.account_id,
    };
    if provider == "codex" && cred.account_id.is_none() {
        return Err(AppError::bad_request().with_param("oauth_account_id_missing"));
    }
    let plaintext = cred.to_plaintext();

    let (channel_id, channel_key_id) = if let Some(channel_id) = req.channel_id {
        let key_id = okapi_store::admin::add_channel_key(
            &state.pg,
            channel_id,
            &plaintext,
            okapi_store::admin::CREDENTIAL_KIND_OAUTH,
            state.master_key.as_deref(),
        )
        .await?;
        (channel_id, key_id)
    } else {
        let name = req
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::bad_request().with_param("name"))?;
        if req.models.is_empty() {
            return Err(AppError::bad_request().with_param("models"));
        }
        let api_base = match provider.as_str() {
            "anthropic_max" => anthropic_max::DEFAULT_API_BASE,
            _ => codex::DEFAULT_API_BASE,
        };
        let models: Vec<&str> = req.models.iter().map(String::as_str).collect();
        let (channel_id, key_id) = okapi_store::provision::create_channel(
            &state.pg,
            name,
            &provider,
            api_base,
            &plaintext,
            &models,
            false,
            state.master_key.as_deref(),
        )
        .await?;
        let mut settings = json!({});
        if let Some(url) = token_url {
            settings["oauth_token_url"] = json!(url);
        }
        sqlx::query!(
            r#"UPDATE channels SET settings = $2 WHERE id = $1"#,
            channel_id,
            settings
        )
        .execute(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        sqlx::query!(
            r#"UPDATE channel_keys SET credential_kind = $2 WHERE id = $1"#,
            key_id,
            okapi_store::admin::CREDENTIAL_KIND_OAUTH
        )
        .execute(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        okapi_store::admin::set_channel_owner(&state.pg, channel_id, actor.user_id).await?;
        (channel_id, key_id)
    };
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.oauth_login",
        &channel_id.to_string(),
        json!({ "provider": provider, "channel_key_id": channel_key_id,
                "expires_at": cred.expires_at, "account_id": cred.account_id }),
    )
    .await;
    Ok(Json(json!({
        "channel_id": channel_id,
        "channel_key_id": channel_key_id,
        "provider": provider,
        "expires_at": cred.expires_at,
        "account_id": cred.account_id,
    })))
}
