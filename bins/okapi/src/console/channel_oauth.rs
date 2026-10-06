//! 渠道 OAuth 登录流程（IMPLEMENTATION §11.38）：站长把自己的 Claude Pro/Max / ChatGPT 订阅登录
//! 成一条 `anthropic_max` / `codex` 渠道。
//!
//! 两步：`start` 生成 PKCE 并把 verifier 存 Redis（10min，一次性），回授权 URL；站长在浏览器里
//! 登录，把回调页 / 地址栏里的 code 贴回 `exchange`，这里换 token → 建渠道或重新授权原 key。
//! 不监听本地回调端口：网关多半跑在服务器上、浏览器在站长电脑上，贴回 code 对所有形态都成立。

use super::admin::{audit, ensure_channel_owner, guard_scoped};
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_providers::UpstreamError;
use okapi_providers::account::{AccountHooks, ExchangeContext};
use okapi_providers::oauth::{Pkce, Tokens};
use okapi_store::credential::OAuthCredential;
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::{Value, json};

const STATE_TTL_SECS: i64 = 600;

/// Public handlers coordinate ownership/state/persistence; the account plugin owns wire behavior.
fn authorization_hook(provider: &str) -> Result<&'static dyn AccountHooks, AppError> {
    okapi_providers::registry::lookup(provider)
        .and_then(|adapter| adapter.account)
        .filter(|hook| hook.capabilities().authorization.is_some())
        .ok_or_else(|| AppError::bad_request().with_param("provider"))
}

#[derive(Deserialize)]
pub struct StartReq {
    pub provider: String,
    #[serde(default)]
    pub channel_id: Option<i64>,
    #[serde(default)]
    pub channel_key_id: Option<i64>,
}

/// POST /admin/channels/oauth/start → `{authorize_url, state, redirect_hint}`。
pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<StartReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let hook = authorization_hook(&req.provider)?;
    if req.channel_key_id.is_some() && req.channel_id.is_none() {
        return Err(AppError::bad_request().with_param("channel_id"));
    }
    if let Some(channel) = req.channel_id {
        ensure_channel_owner(&state, channel, &actor, scope).await?;
        if req.channel_key_id.is_none() {
            return Err(AppError::bad_request().with_param("channel_key_id"));
        }
        if let Some(key) = req.channel_key_id {
            let target = okapi_store::oauth_credentials::target(&state.pg, channel, key)
                .await?
                .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
            if target.provider != req.provider {
                return Err(AppError::bad_request().with_param("channel_provider_mismatch"));
            }
        }
    }
    let pkce = Pkce::generate()
        .map_err(|_| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, codes::INTERNAL_ERROR))?;
    let nonce: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    let authorization = hook
        .authorize(&pkce, &nonce)
        .ok_or_else(|| AppError::bad_request().with_param("provider"))?;
    let stored = json!({ "provider": req.provider, "verifier": pkce.verifier,
        "actor_id":actor.user_id, "channel_id":req.channel_id, "channel_key_id":req.channel_key_id });
    if !state
        .sched
        .oauth_cred_state_set(&authorization.state, &stored.to_string(), STATE_TTL_SECS)
        .await
    {
        return Err(AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL_ERROR,
        ));
    }
    Ok(Json(json!({
        "provider": req.provider,
        "authorize_url": authorization.url,
        "state": authorization.state,
        "redirect_uri": authorization.redirect_uri,
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
    /// Reauthorize an existing channel; channel_key_id is required.
    #[serde(default)]
    pub channel_id: Option<i64>,
    /// Replace this key after reauthorization; must be bound into the issued state.
    #[serde(default)]
    pub channel_key_id: Option<i64>,
    /// token 端点覆写（测试 mock / 企业代理）；落 `settings.oauth_token_url`。
    #[serde(default)]
    pub token_url: Option<String>,
    /// Empty uses the provider registration's default endpoint.
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(flatten)]
    pub options: super::channel_creation::Options,
}

/// POST /admin/channels/oauth/exchange → 换 token、建渠道 / 重新授权原 key。
// 换码 → 凭证 → 建渠道 / 加 key 的线性时序放同一视野
#[allow(clippy::too_many_lines)]
pub async fn exchange(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<ExchangeReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let stored = state
        .sched
        .oauth_cred_state_take(&req.state)
        .await
        .ok_or_else(|| AppError::new(StatusCode::BAD_REQUEST, "oauth_state_invalid"))?;
    let stored: Value = serde_json::from_str(&stored)
        .map_err(|_| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, codes::INTERNAL_ERROR))?;
    let provider = stored["provider"].as_str().unwrap_or_default().to_owned();
    let verifier = stored["verifier"].as_str().unwrap_or_default().to_owned();
    let hook = authorization_hook(&provider)?;
    if stored["actor_id"].as_i64() != Some(actor.user_id)
        || stored["channel_id"]
            .as_i64()
            .is_some_and(|id| Some(id) != req.channel_id)
        || stored["channel_key_id"].as_i64() != req.channel_key_id
        || req.channel_key_id.is_some() && req.channel_id.is_none()
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "oauth_state_invalid",
        ));
    }
    let prepared = if req.channel_id.is_none() {
        Some(super::channel_creation::prepare(&state, &provider, req.options).await?)
    } else {
        super::admin::ensure_max_concurrency(req.options.max_concurrency)?;
        super::admin::validate_channel_settings(&state, &provider, req.options.settings.as_ref())
            .await?;
        None
    };
    let api_base = if prepared.is_some() {
        if req
            .name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
        {
            return Err(AppError::bad_request().with_param("name"));
        }
        if req.models.is_empty() {
            return Err(AppError::bad_request().with_param("models"));
        }
        Some(super::channel_creation::endpoint(&state, &provider, req.api_base.as_deref()).await?)
    } else {
        None
    };
    let mut token_url = req
        .token_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    // 更新既有渠道：属主范围与 provider 一致性在换码**之前**判——授权码一次性，
    // 被拒的请求不该先把它烧掉；own 范围的渠道管理员也不能往别人的渠道里塞 key。
    // 换码就从账号日后的出口出去（§11.41）：新建渠道按待落的绑定选出口——固定分配组当场选定
    // 代理，建渠道时直接落这个分配；重新授权沿用这把 key 现在的出口。出口不可用即 409，不直连。
    let mut proxy_url: Option<String> = None;
    let mut egress_preassigned = None;
    if let Some(prepared) = prepared.as_ref() {
        super::egress::validate_binding(&state, &prepared.egress, &actor, scope).await?;
        let (resolved, preassigned) = okapi_store::egress::pick_for_new_key(
            &state.pg,
            &prepared.egress,
            state.master_key.as_deref(),
        )
        .await?;
        proxy_url = resolved.proxy_url()?;
        egress_preassigned = preassigned;
    }
    if token_url.is_none() {
        token_url = prepared
            .as_ref()
            .and_then(|prepared| prepared.settings.get("oauth_token_url"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    let mut reauthorization_target = None;
    if let Some(channel_id) = req.channel_id {
        ensure_channel_owner(&state, channel_id, &actor, scope).await?;
        let existing = sqlx::query!(
            r#"SELECT provider, settings FROM channels WHERE id = $1 AND deleted_at IS NULL"#,
            channel_id
        )
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
        if existing.provider != provider {
            return Err(AppError::bad_request().with_param("channel_provider_mismatch"));
        }
        if req.channel_key_id.is_none() {
            return Err(AppError::bad_request().with_param("channel_key_id"));
        }
        if token_url.is_none() {
            token_url = existing
                .settings
                .get("oauth_token_url")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty());
        }
        if let Some(key) = req.channel_key_id {
            reauthorization_target = Some(
                okapi_store::oauth_credentials::target(&state.pg, channel_id, key)
                    .await?
                    .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?,
            );
            proxy_url = super::egress::key_proxy(&state, key).await?;
        }
    }

    if let Some(url) = token_url.as_deref() {
        super::ssrf::validate_api_base(&state, url).await?;
    }
    let http = state.upstream.http();
    let tokens: Tokens = hook
        .exchange(ExchangeContext {
            http,
            token_url: token_url.as_deref(),
            pasted_code: &req.code,
            verifier: &verifier,
            proxy_url: proxy_url.as_deref(),
        })
        .await
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
        expires_at: now.saturating_add(tokens.expires_in),
        account_id: tokens.account_id,
        account_label: tokens.account_label,
    };
    if hook
        .capabilities()
        .authorization
        .is_some_and(|rules| rules.account_id_required)
        && cred
            .account_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
    {
        return Err(AppError::bad_request().with_param("oauth_account_id_missing"));
    }
    if cred.access_token.trim().is_empty()
        || cred.refresh_token.trim().is_empty()
        || tokens.expires_in <= 0
    {
        return Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "oauth_invalid_token_response",
        ));
    }
    let plaintext = cred.to_plaintext();

    let (channel_id, channel_key_id) = if let Some(channel_id) = req.channel_id {
        ensure_channel_owner(&state, channel_id, &actor, scope).await?;
        let key_id = if let Some(key_id) = req.channel_key_id {
            let target = reauthorization_target
                .as_ref()
                .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
            if target.provider != provider {
                return Err(AppError::bad_request().with_param("channel_provider_mismatch"));
            }
            let old = okapi_store::credential::open(
                state.master_key.as_deref(),
                &target.credential_ciphertext,
            )?;
            if OAuthCredential::parse(&old)
                .and_then(|old| old.account_id)
                .is_some_and(|account| Some(account) != cred.account_id)
            {
                return Err(AppError::new(
                    StatusCode::CONFLICT,
                    "oauth_account_mismatch",
                ));
            }
            if !okapi_store::oauth_credentials::reauthorize(
                &state.pg,
                channel_id,
                key_id,
                &provider,
                &target.credential_ciphertext,
                &plaintext,
                state.master_key.as_deref(),
            )
            .await?
            {
                return Err(AppError::new(
                    StatusCode::CONFLICT,
                    "oauth_credential_changed",
                ));
            }
            key_id
        } else {
            return Err(AppError::bad_request().with_param("channel_key_id"));
        };
        (channel_id, key_id)
    } else {
        let mut prepared = prepared.ok_or_else(AppError::internal)?;
        if let Some(url) = token_url {
            prepared.settings["oauth_token_url"] = json!(url);
        }
        let models: Vec<&str> = req.models.iter().map(String::as_str).collect();
        okapi_store::provision::create_channel_configured(
            &state.pg,
            okapi_store::provision::ChannelCreate {
                name: req.name.as_deref().ok_or_else(AppError::internal)?.trim(),
                provider: &provider,
                api_base: api_base.ok_or_else(AppError::internal)?,
                credential: &plaintext,
                models: &models,
                trust_upstream_usage: false,
                owner_id: Some(actor.user_id),
                settings: Some(&prepared.settings),
                priority: prepared.priority,
                max_concurrency: prepared.max_concurrency,
                cost_milli: prepared.cost_milli,
                pools: Some(&prepared.pools),
                egress: Some(&prepared.egress),
                egress_preassigned,
            },
            state.master_key.as_deref(),
        )
        .await?
    };
    state.invalidate_routing_caches();
    crate::gateway::credentials::health::record(&state, channel_key_id, None).await;
    audit(
        &state,
        &actor,
        if req.channel_key_id.is_some() {
            "channel.oauth_reauthorize"
        } else {
            "channel.oauth_login"
        },
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

/// POST /admin/channels/{id}/keys/{key}/oauth/refresh. Does not reset scheduling status.
pub async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((channel, key)): Path<(i64, i64)>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel, &actor, scope).await?;
    let row = okapi_store::oauth_credentials::target(&state.pg, channel, key)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    let plain =
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
    let token_url = row.settings.get("oauth_token_url").and_then(Value::as_str);
    if let Some(url) = token_url {
        super::ssrf::validate_api_base(&state, url).await?;
    }
    let proxy = super::egress::key_proxy(&state, key).await?;
    let credential = crate::gateway::credentials::oauth::force_refresh_for(
        &state,
        &crate::gateway::credentials::oauth::OAuthKey {
            channel_key_id: key,
            provider: &row.provider,
            token_url,
            proxy_url: proxy.as_deref(),
        },
        &plain,
    )
    .await
    .map_err(|error| {
        AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR).with_param(error.error_code())
    })?;
    audit(
        &state,
        &actor,
        "channel.oauth_refresh",
        &channel.to_string(),
        json!({"channel_key_id":key, "expires_at":credential.expires_at}),
    )
    .await;
    Ok(Json(json!({"ok":true, "expires_at":credential.expires_at})))
}
