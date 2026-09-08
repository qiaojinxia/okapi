//! 订阅 OAuth 凭证的取用与刷新（IMPLEMENTATION §11.38；四步锁见 §4.3）。
//!
//! 候选行里的 `credential` 是凭证 JSON 原文（`OAuthCredential`）。请求路径上惰性刷新：
//! 到期前 120s 即刷；刷新按 进程内单飞 → Redis `lock:cred` → 加锁后重读 DB → 刷新并回写
//! 四步走，`invalid_grant` 二次重读后仍失败则把 key 置 invalid（仅人工重登可恢复）。
//! 刷新失败但旧 token 还没过期时先用旧的——上游 token 端点抖一下不该让请求失败。

use super::state::AppState;
use okapi_providers::UpstreamError;
use okapi_providers::oauth::{Tokens, anthropic_max, codex, is_invalid_grant};
use okapi_store::channels::{ChannelCandidate, KeyFailure};
use okapi_store::credential::OAuthCredential;
use std::collections::HashMap;
use std::sync::Arc;

/// 到期前多久算"该刷了"：一次长流式请求也不该撞上过期。
const REFRESH_MARGIN_SECS: i64 = 120;

/// 进程内单飞：同一把 key 的并发请求只让一个去刷，其余等它的结果。
#[derive(Default)]
pub struct RefreshGate {
    inner: tokio::sync::Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

impl RefreshGate {
    async fn key_mutex(&self, channel_key_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.inner.lock().await;
        Arc::clone(map.entry(channel_key_id).or_default())
    }
}

/// 该 provider 是否用订阅 OAuth 凭证。
#[must_use]
pub fn is_oauth_provider(provider: &str) -> bool {
    matches!(provider, "anthropic_max" | "codex")
}

/// 真实 Claude Code / Codex CLI 经网关出去时原样带到上游的客户端身份头（§11.38）。
/// 网关不编造这些头：客户端带了就转发，没带就没有。
const CLIENT_HEADERS: [&str; 13] = [
    "user-agent",
    "accept-language",
    // Anthropic 族
    "x-app",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    // Codex 族
    "originator",
    "version",
    "session_id",
    "conversation_id",
    "openai-beta",
    "x-codex-beta-features",
    "x-codex-installation-id",
    "x-codex-window-id",
];
const CLIENT_HEADER_PREFIXES: [&str; 2] = ["x-stainless-", "x-codex-turn-"];

/// 从入口请求里挑出可透传的身份头（只在订阅 provider 的出向上生效；受保护头由
/// `okapi_providers::http::is_forbidden_header` 在写入时再拦一次）。
#[must_use]
pub fn client_headers(headers: &axum::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| {
            let name = name.as_str();
            CLIENT_HEADERS.contains(&name)
                || CLIENT_HEADER_PREFIXES.iter().any(|p| name.starts_with(p))
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_owned(), v.to_owned()))
        })
        .collect()
}

/// 订阅渠道的出向修饰 = 渠道设置里的代理 / 额外头 + 客户端身份头；其它渠道原样。
#[must_use]
pub fn outbound_with_client(
    cand: &ChannelCandidate,
    client: &[(String, String)],
) -> okapi_providers::Outbound {
    let mut outbound = super::openai_dialect::outbound(cand);
    if is_oauth_provider(&cand.provider) {
        outbound.extra_headers.extend(client.iter().cloned());
    }
    outbound
}

/// 刷新所需的最小上下文：候选行或管理面探测都能凑出来。
pub struct OAuthKey<'a> {
    pub channel_key_id: i64,
    pub provider: &'a str,
    /// `settings.oauth_token_url` 覆写；None = 官方地址。
    pub token_url: Option<&'a str>,
    /// 渠道代理：刷新得和 API 请求走同一个出口（订阅账号对出口 IP 敏感，
    /// 只有代理能出网的部署也才刷得动）。
    pub proxy_url: Option<&'a str>,
}

impl<'a> From<&'a ChannelCandidate> for OAuthKey<'a> {
    fn from(cand: &'a ChannelCandidate) -> Self {
        Self {
            channel_key_id: cand.channel_key_id,
            provider: &cand.provider,
            token_url: cand.oauth_token_url.as_deref(),
            proxy_url: cand.proxy_url.as_deref(),
        }
    }
}

/// 从候选里取出一份此刻可用的 OAuth 凭证（必要时刷新并回写）。
/// 凭证不是 OAuth 形态 → 构造错误（渠道配错了凭证）。
pub async fn fresh_credential(
    state: &AppState,
    cand: &ChannelCandidate,
) -> Result<OAuthCredential, UpstreamError> {
    fresh_credential_for(state, &OAuthKey::from(cand), &cand.credential).await
}

/// 同上，输入为原始字段（管理面探测用）。
pub async fn fresh_credential_for(
    state: &AppState,
    key: &OAuthKey<'_>,
    credential_plaintext: &str,
) -> Result<OAuthCredential, UpstreamError> {
    let current = OAuthCredential::parse(credential_plaintext)
        .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".to_owned()))?;
    let now = chrono::Utc::now().timestamp();
    if !current.needs_refresh(now, REFRESH_MARGIN_SECS) {
        return Ok(current);
    }
    refresh_with_lock(state, key, current, now).await
}

async fn refresh_with_lock(
    state: &AppState,
    key: &OAuthKey<'_>,
    expired: OAuthCredential,
    now: i64,
) -> Result<OAuthCredential, UpstreamError> {
    // 1. 进程内单飞
    let gate = state.refresh_gate.key_mutex(key.channel_key_id).await;
    let _local = gate.lock().await;
    // 2. Redis 分布式锁；拿不到 = 他副本正在刷，等一小会再重读
    let locked = state.sched.cred_lock_acquire(key.channel_key_id).await;
    if !locked {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    // 3. 加锁后重读 DB：别的 pod / 刚才拿锁的副本可能已经刷好
    let current_db = reread(state, key.channel_key_id).await;
    if let Some(fresh) = current_db
        .as_ref()
        .filter(|c| !c.needs_refresh(now, REFRESH_MARGIN_SECS))
    {
        if locked {
            state.sched.cred_lock_release(key.channel_key_id).await;
        }
        return Ok(fresh.clone());
    }
    if !locked {
        // 等了一轮仍没刷好：没锁不该自己动手，旧 token 未过期就先用，否则报瞬态错让重试矩阵处理
        return stale_or_err(expired, now);
    }
    let basis = current_db.unwrap_or_else(|| expired.clone());
    // 4. 刷新 + 回写
    let outcome = do_refresh(state, key, &basis).await;
    let result = match outcome {
        Ok(tokens) => {
            let next = OAuthCredential {
                access_token: tokens.access_token,
                refresh_token: tokens
                    .refresh_token
                    .unwrap_or_else(|| basis.refresh_token.clone()),
                expires_at: now + tokens.expires_in,
                account_id: tokens.account_id.or_else(|| basis.account_id.clone()),
            };
            match okapi_store::admin::write_key_credential(
                &state.pg,
                key.channel_key_id,
                &next.to_plaintext(),
                state.master_key.as_deref(),
            )
            .await
            {
                Ok(()) => state.invalidate_routing_caches(),
                Err(err) => {
                    tracing::error!(error = %err, key = key.channel_key_id, "OAuth 凭证回写失败（本次仍用新 token）");
                }
            }
            Ok(next)
        }
        Err(UpstreamError::Status { status, body, .. }) if is_invalid_grant(status, &body) => {
            // 竞争恢复：refresh token 可能刚被别的副本用掉并轮转，再读一次
            match reread(state, key.channel_key_id).await {
                Some(c) if c.refresh_token != basis.refresh_token => Ok(c),
                _ => {
                    tracing::warn!(
                        key = key.channel_key_id,
                        status,
                        "OAuth refresh token 已失效，key 置 invalid"
                    );
                    let _ = okapi_store::channels::mark_key_failure(
                        &state.pg,
                        key.channel_key_id,
                        "oauth_invalid_grant",
                        KeyFailure::Invalid,
                    )
                    .await;
                    state.invalidate_routing_caches();
                    Err(UpstreamError::Status {
                        status: 401,
                        body,
                        retry_after_secs: None,
                    })
                }
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, key = key.channel_key_id, "OAuth 刷新瞬态失败");
            stale_or_err(expired, now).map_err(|_| err)
        }
    };
    state.sched.cred_lock_release(key.channel_key_id).await;
    result
}

fn stale_or_err(stale: OAuthCredential, now: i64) -> Result<OAuthCredential, UpstreamError> {
    if stale.expires_at > now {
        Ok(stale)
    } else {
        Err(UpstreamError::Connect(
            "oauth_refresh_unavailable".to_owned(),
        ))
    }
}

async fn reread(state: &AppState, channel_key_id: i64) -> Option<OAuthCredential> {
    okapi_store::admin::read_key_credential(&state.pg, channel_key_id, state.master_key.as_deref())
        .await
        .ok()
        .flatten()
        .and_then(|plain| OAuthCredential::parse(&plain))
}

async fn do_refresh(
    state: &AppState,
    key: &OAuthKey<'_>,
    basis: &OAuthCredential,
) -> Result<Tokens, UpstreamError> {
    let http = state.upstream.http();
    match key.provider {
        "anthropic_max" => {
            anthropic_max::refresh(
                http,
                key.token_url.unwrap_or(anthropic_max::TOKEN_URL),
                &basis.refresh_token,
                key.proxy_url,
            )
            .await
        }
        "codex" => {
            codex::refresh(
                http,
                key.token_url.unwrap_or(codex::TOKEN_URL),
                &basis.refresh_token,
                key.proxy_url,
            )
            .await
        }
        other => Err(UpstreamError::Build(format!("not_oauth_provider:{other}"))),
    }
}
