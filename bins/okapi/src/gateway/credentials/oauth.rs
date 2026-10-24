//! 订阅 OAuth 凭证的取用与刷新（IMPLEMENTATION §11.38；四步锁见 §4.3）。
//!
//! 候选行里的 `credential` 是凭证 JSON 原文（`OAuthCredential`）。请求路径上惰性刷新：
//! 到期前 120s 即刷；刷新按 进程内单飞 → Redis `lock:cred` → 加锁后重读 DB → 刷新并回写
//! 四步走，`invalid_grant` 二次重读后仍失败则把 key 置 invalid（仅人工重登可恢复）。
//! 刷新失败但旧 token 还没过期时先用旧的——上游 token 端点抖一下不该让请求失败。
//!
//! 新凭据落盘三次都失败时，暂存进 Redis（各副本可见）和本进程内存（见 [`super::pending`]），
//! 任一副本下次进刷新临界区先补写它、写不进去也先用它，而不是拿已作废的旧 refresh token 再去刷。
//! 没盖住的情况：Redis 与本进程同时丢了这份（Redis 不可用时进程又在补写前退出），或 24 小时内都没
//! 补写成功；上游已经轮换了 refresh token、应答却没回到我们这里（45 秒超时、连接中断）——新凭据
//! 从没到手，下次刷新只会撞 invalid_grant，账号要人工重登。

use super::super::state::AppState;
use okapi_providers::UpstreamError;
use okapi_providers::oauth::{Tokens, is_invalid_grant};
use okapi_store::channels::ChannelCandidate;
use okapi_store::credential::OAuthCredential;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// 到期前多久算"该刷了"：一次长流式请求也不该撞上过期。
const REFRESH_MARGIN_SECS: i64 = 120;
/// 刷新临界区里单次数据库读 / 写的上限。租约只有 90 秒（刷新成功后还会续满），所以临界区里的每一步
/// 都得有界：上游 HTTP 最长 45 秒，前面的重读 ≤ 10 秒，后面的写最多尝试三次、每次 ≤ 10 秒。
/// 不设上限的话，一次数据库卡死就能让租约过期，别的副本拿同一个 refresh token 再刷一遍。
const STORE_STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// 刷新成功后新凭据落盘失败的重试间隔（共三次尝试）。
const WRITE_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(200), Duration::from_millis(800)];

/// 失败就按 `delays` 的间隔重试（尝试次数 = `delays.len() + 1`），返回最后一次的结果。
async fn with_retries<T, E, Fut>(
    delays: &[Duration],
    mut attempt: impl FnMut() -> Fut,
) -> Result<T, E>
where
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut tried = 0;
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => match delays.get(tried) {
                Some(delay) => {
                    tokio::time::sleep(*delay).await;
                    tried += 1;
                }
                None => return Err(error),
            },
        }
    }
}

/// 进程内单飞：同一把 key 的并发请求只让一个去刷，其余等它的结果。
#[derive(Default)]
pub struct RefreshGate {
    inner: tokio::sync::Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    /// 刷新成功、落盘却失败的新凭据在本进程的那一份（见 [`super::pending`]）。
    pub(super) pending: super::pending::Memory,
}

impl RefreshGate {
    async fn key_mutex(&self, channel_key_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.inner.lock().await;
        // 按 key 只增不减会随 key 的增删慢慢涨：表大了就清掉没人持有的锁。正在刷新或排队等
        // 结果的调用方各持一份 Arc，计数大于 1 的不会被清，单飞语义不受影响。
        if map.len() >= 256 {
            map.retain(|_, gate| Arc::strong_count(gate) > 1);
        }
        Arc::clone(map.entry(channel_key_id).or_default())
    }
}

/// 该 provider 是否用订阅 OAuth 凭证。
#[must_use]
pub fn is_oauth_provider(provider: &str) -> bool {
    okapi_providers::registry::lookup(provider).is_some_and(|adapter| {
        matches!(
            adapter.credential,
            okapi_providers::registry::CredentialKind::OAuth(_)
        )
    })
}

// Compatibility exports; request extensions are independent of OAuth lifecycle.
pub use super::super::extensions::{client_headers, outbound_with_client};

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

/// 拿不到可用订阅凭证是这个账号的问题，不是请求的：统一成可换账号重试的错误。
/// 401 / 429 / 5xx 保留原状态，驱动 key 状态机（失效、限流冷却、连续失败）；其余（token 端点
/// 的 4xx、凭证被并发改写、读库失败……）按连接级失败处理——token 端点的错误体不能当成上游
/// 回答转给客户端，也不能记成客户端的 bad_request。
#[must_use]
pub fn unavailable(error: UpstreamError) -> UpstreamError {
    match error {
        UpstreamError::Status {
            status: 401 | 429 | 500..=599,
            ..
        }
        | UpstreamError::Connect(_)
        | UpstreamError::Unreachable { .. }
        | UpstreamError::Timeout => error,
        UpstreamError::Status { .. }
        | UpstreamError::Stream(_)
        | UpstreamError::Session { .. }
        | UpstreamError::Build(_) => UpstreamError::Connect("oauth_credential_unavailable".into()),
    }
}

/// 从候选里取出一份此刻可用的 OAuth 凭证（必要时刷新并回写）。
/// 凭证不是 OAuth 形态 → 构造错误（渠道配错了凭证）。
pub async fn fresh_credential(
    state: &AppState,
    cand: &ChannelCandidate,
) -> Result<OAuthCredential, UpstreamError> {
    let control = super::super::account_control::policy(state, cand.channel_id).await?;
    let fresh = fresh_with_policy(state, &OAuthKey::from(cand), &cand.credential, &control).await?;
    {
        let original = OAuthCredential::parse(&cand.credential)
            .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".to_owned()))?;
        if original.account_id.is_some() && original.account_id != fresh.account_id {
            // 锁内重读/刷新可能遇到管理员重登另一个账号；当前请求不能被移交。
            return Err(UpstreamError::Build("oauth_account_changed".to_owned()));
        }
    }
    Ok(fresh)
}

/// 同上，输入为原始字段（管理面探测用）。
pub async fn fresh_credential_for(
    state: &AppState,
    key: &OAuthKey<'_>,
    credential_plaintext: &str,
) -> Result<OAuthCredential, UpstreamError> {
    let control = control_for(state, key.channel_key_id).await?;
    fresh_with_policy(state, key, credential_plaintext, &control).await
}

async fn fresh_with_policy(
    state: &AppState,
    key: &OAuthKey<'_>,
    credential_plaintext: &str,
    control: &super::super::account_control::policy::Policy,
) -> Result<OAuthCredential, UpstreamError> {
    let current = OAuthCredential::parse(credential_plaintext)
        .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".into()))?;
    if control.refresh_mode == super::super::account_control::policy::RefreshMode::External {
        if current.expires_at > 0 && current.expires_at <= chrono::Utc::now().timestamp() {
            return Err(UpstreamError::Status {
                status: 401,
                body: bytes::Bytes::new(),
                retry_after_secs: None,
            });
        }
        return Ok(current);
    }
    if !current.needs_refresh(chrono::Utc::now().timestamp(), control.refresh_margin_secs) {
        return Ok(current);
    }
    refresh_with_lock(state, key, None, control.refresh_margin_secs).await
}

async fn control_for(
    state: &AppState,
    key: i64,
) -> Result<Arc<super::super::account_control::policy::Policy>, UpstreamError> {
    let channel: i64 = sqlx::query_scalar("SELECT channel_id FROM channel_keys WHERE id=$1")
        .bind(key)
        .fetch_one(&state.pg)
        .await
        .map_err(|_| UpstreamError::Build("oauth_credential_read_failed".into()))?;
    super::super::account_control::policy(state, channel).await
}

/// Worker pre-refresh uses the same lease, reread and CAS flow with an earlier margin.
pub async fn refresh_due(
    state: &AppState,
    key: &OAuthKey<'_>,
    plaintext: &str,
    margin_secs: i64,
) -> Result<OAuthCredential, UpstreamError> {
    if control_for(state, key.channel_key_id).await?.refresh_mode
        == super::super::account_control::policy::RefreshMode::External
    {
        return Err(UpstreamError::Build("oauth_refresh_not_available".into()));
    }
    let current = OAuthCredential::parse(plaintext)
        .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".to_owned()))?;
    let now = chrono::Utc::now().timestamp();
    if !current.needs_refresh(now, margin_secs) {
        return Ok(current);
    }
    refresh_with_lock(state, key, None, margin_secs).await
}

pub async fn force_refresh_for(
    state: &AppState,
    key: &OAuthKey<'_>,
    plaintext: &str,
) -> Result<OAuthCredential, UpstreamError> {
    let current = OAuthCredential::parse(plaintext)
        .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".to_owned()))?;
    if !current.can_refresh() {
        return Err(UpstreamError::Build("oauth_refresh_not_available".into()));
    }
    let rejected = current.access_token.clone();
    refresh_with_lock(state, key, Some(&rejected), REFRESH_MARGIN_SECS).await
}

/// A 401 before output proves this access token was rejected. Refresh it once,
/// accepting a concurrently rotated token instead of refreshing it again.
pub async fn refresh_rejected_credential(
    state: &AppState,
    cand: &ChannelCandidate,
) -> Result<OAuthCredential, UpstreamError> {
    let current = OAuthCredential::parse(&cand.credential)
        .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".to_owned()))?;
    let rejected = current.access_token.clone();
    let account = current.account_id.clone();
    let fresh = refresh_with_lock(
        state,
        &OAuthKey::from(cand),
        Some(&rejected),
        REFRESH_MARGIN_SECS,
    )
    .await?;
    if account.is_some() && fresh.account_id != account {
        return Err(UpstreamError::Build("oauth_account_changed".to_owned()));
    }
    Ok(fresh)
}

async fn refresh_with_lock(
    state: &AppState,
    key: &OAuthKey<'_>,
    rejected_token: Option<&str>,
    margin_secs: i64,
) -> Result<OAuthCredential, UpstreamError> {
    if control_for(state, key.channel_key_id).await?.refresh_mode
        == super::super::account_control::policy::RefreshMode::External
    {
        return Err(UpstreamError::Build("oauth_refresh_not_available".into()));
    }
    let gate = state.refresh_gate.key_mutex(key.channel_key_id).await;
    let _local = gate.lock().await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    let owner = loop {
        let acquired = state.sched.cred_lock_acquire(key.channel_key_id).await;
        if let Ok(Some(owner)) = &acquired {
            break owner.clone();
        }
        let current = reread(state, key.channel_key_id).await?;
        let now = chrono::Utc::now().timestamp();
        if !matches!(current.status, 1..=3) {
            return Err(UpstreamError::Status {
                status: 401,
                body: bytes::Bytes::new(),
                retry_after_secs: None,
            });
        }
        if !current.credential.needs_refresh(now, margin_secs)
            && rejected_token.is_none_or(|token| current.credential.access_token != token)
        {
            return Ok(current.credential);
        }
        if rejected_token.is_none() && current.credential.expires_at > now {
            return Ok(current.credential);
        }
        if acquired.is_err() || tokio::time::Instant::now() >= deadline {
            return Err(UpstreamError::Timeout);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    // Keep the refresh I/O future off the stack of every gateway request.
    let result = Box::pin(refresh_locked(
        state,
        key,
        chrono::Utc::now().timestamp(),
        rejected_token,
        margin_secs,
        &owner,
    ))
    .await;
    state
        .sched
        .cred_lock_release(key.channel_key_id, &owner)
        .await;
    result
}

struct CurrentCredential {
    credential: OAuthCredential,
    stored: Vec<u8>,
    status: i16,
}

async fn reread(state: &AppState, key: i64) -> Result<CurrentCredential, UpstreamError> {
    let snapshot = tokio::time::timeout(
        STORE_STEP_TIMEOUT,
        okapi_store::oauth_credentials::snapshot(&state.pg, key, state.master_key.as_deref()),
    )
    .await
    .map_err(|_| UpstreamError::Build("oauth_credential_read_failed".into()))?
    .map_err(|_| UpstreamError::Build("oauth_credential_read_failed".into()))?
    .ok_or_else(|| UpstreamError::Build("oauth_credential_missing".into()))?;
    Ok(CurrentCredential {
        credential: OAuthCredential::parse(&snapshot.plaintext)
            .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".into()))?,
        stored: snapshot.stored,
        status: snapshot.status,
    })
}

async fn replaced_credential(
    state: &AppState,
    key: i64,
    expected: &[u8],
) -> Result<OAuthCredential, UpstreamError> {
    let current = reread(state, key).await?;
    if current.stored == expected || !matches!(current.status, 1..=3) {
        return Err(UpstreamError::Build("oauth_credential_changed".into()));
    }
    Ok(current.credential)
}

// Lease is released by the caller on every result; cancellation is healed by lease expiry.
#[allow(clippy::too_many_lines)]
async fn refresh_locked(
    state: &AppState,
    key: &OAuthKey<'_>,
    now: i64,
    rejected: Option<&str>,
    margin_secs: i64,
    owner: &str,
) -> Result<OAuthCredential, UpstreamError> {
    let current = reread(state, key.channel_key_id).await?;
    if !matches!(current.status, 1..=3) {
        super::pending::clear(state, key.channel_key_id).await;
        return Err(UpstreamError::Status {
            status: 401,
            body: bytes::Bytes::new(),
            retry_after_secs: None,
        });
    }
    let basis = &current.credential;
    if !basis.needs_refresh(now, margin_secs)
        && rejected.is_none_or(|token| basis.access_token != token)
    {
        return Ok(basis.clone());
    }
    if let Some(next) = super::pending::take(state, key.channel_key_id, &current.stored).await {
        return save_refreshed(state, key, &current.stored, next).await;
    }
    if rejected.is_none()
        && super::health::read(state, key.channel_key_id)
            .await
            .is_some_and(|health| !health.retry_ready(now))
    {
        return stale_or_err(basis.clone(), now);
    }
    if basis.refresh_token.trim().is_empty() {
        let code = if rejected.is_some() {
            "oauth_access_token_rejected"
        } else {
            "oauth_access_token_expired"
        };
        return invalidate_current(state, key, &current.stored, code).await;
    }
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        do_refresh(state, key, basis),
    )
    .await
    .unwrap_or(Err(UpstreamError::Timeout));
    match outcome {
        Ok(tokens) => {
            // 旧 refresh token 此刻已在上游作废：到落盘完成之前租约不能过期、被别的副本抢走
            if !state
                .sched
                .cred_lock_extend(key.channel_key_id, owner)
                .await
            {
                tracing::warn!(
                    key = key.channel_key_id,
                    "OAuth 刷新租约在刷新期间已丢失，仍按 CAS 落盘"
                );
            }
            let next = OAuthCredential {
                access_token: tokens.access_token,
                refresh_token: tokens
                    .refresh_token
                    .unwrap_or_else(|| basis.refresh_token.clone()),
                expires_at: chrono::Utc::now()
                    .timestamp()
                    .saturating_add(tokens.expires_in),
                account_id: tokens.account_id.or_else(|| basis.account_id.clone()),
                account_label: tokens.account_label.or_else(|| basis.account_label.clone()),
                // 刷新响应不带 scope 时沿用旧值：下次刷新被拒 invalid_scope 要靠它判断能不能降级
                scope: tokens.scope.or_else(|| basis.scope.clone()),
            };
            if basis.account_id.is_some() && next.account_id != basis.account_id {
                return invalidate_current(state, key, &current.stored, "oauth_account_changed")
                    .await;
            }
            if next.access_token.trim().is_empty() || tokens.expires_in <= 0 {
                super::health::record(
                    state,
                    key.channel_key_id,
                    Some("oauth_invalid_token_response"),
                )
                .await;
                // 与显式失败同一兜底：旧 access token 还没过期、也没被上游拒过，就先用旧的
                let invalid = || UpstreamError::Build("oauth_invalid_token_response".into());
                if rejected.is_some() {
                    return Err(invalid());
                }
                return stale_or_err(basis.clone(), chrono::Utc::now().timestamp())
                    .map_err(|_| invalid());
            }
            save_refreshed(state, key, &current.stored, next).await
        }
        Err(UpstreamError::Status { status, body, .. }) if is_invalid_grant(status, &body) => {
            invalidate_current(state, key, &current.stored, "oauth_invalid_grant").await
        }
        Err(error) => {
            let latest = reread(state, key.channel_key_id).await?;
            if latest.stored != current.stored {
                return replaced_credential(state, key.channel_key_id, &current.stored).await;
            }
            let code = error.upstream_status().map_or_else(
                || error.error_code().to_owned(),
                |status| format!("oauth_refresh_status_{status}"),
            );
            tracing::warn!(key = key.channel_key_id, error_code = %code, "OAuth refresh failed");
            super::health::record(state, key.channel_key_id, Some(&code)).await;
            if rejected.is_some() {
                Err(error)
            } else {
                stale_or_err(basis.clone(), chrono::Utc::now().timestamp()).map_err(|_| error)
            }
        }
    }
}

/// 把刷新得到的新凭据以 `expected` 为旧值 CAS 落盘。
///
/// 上游已经消费了旧 refresh token（Codex 等强制单次轮换），新凭据只活在内存里：这一步遇到瞬时故障
/// （连接抖动、超时）就丢掉的话，下次刷新只会撞 invalid_grant，账号只能人工重登。所以重试几次，
/// 每次都以旧值做 CAS：前一次其实已落盘、只是应答丢了，重试返回 false，`replaced_credential`
/// 会读到我们自己写进去的那份。三次都失败就把新凭据暂存起来（Redis + 本进程，见
/// [`super::pending`]），任一副本下次进临界区先补写，这次照样拿它去用（access token 是有效的）。
async fn save_refreshed(
    state: &AppState,
    key: &OAuthKey<'_>,
    expected: &[u8],
    next: OAuthCredential,
) -> Result<OAuthCredential, UpstreamError> {
    let saved = with_retries(&WRITE_RETRY_DELAYS, || async {
        tokio::time::timeout(
            STORE_STEP_TIMEOUT,
            okapi_store::oauth_credentials::write_if_current(
                &state.pg,
                key.channel_key_id,
                key.provider,
                expected,
                &next.to_plaintext(),
                state.master_key.as_deref(),
            ),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
    })
    .await;
    let Ok(saved) = saved else {
        tracing::error!(
            key = key.channel_key_id,
            "OAuth 刷新已让上游作废旧 refresh token，但新凭据三次落盘都失败；已暂存待补写"
        );
        super::health::record(
            state,
            key.channel_key_id,
            Some("oauth_credential_write_failed"),
        )
        .await;
        let usable = next.expires_at > chrono::Utc::now().timestamp();
        super::pending::put(state, key.channel_key_id, expected, &next).await;
        return if usable {
            Ok(next)
        } else {
            Err(UpstreamError::Build("oauth_credential_write_failed".into()))
        };
    };
    super::pending::clear(state, key.channel_key_id).await;
    if !saved {
        return replaced_credential(state, key.channel_key_id, expected).await;
    }
    state.invalidate_routing_caches();
    super::health::record(state, key.channel_key_id, None).await;
    Ok(next)
}

async fn invalidate_current(
    state: &AppState,
    key: &OAuthKey<'_>,
    expected: &[u8],
    code: &str,
) -> Result<OAuthCredential, UpstreamError> {
    // 临界区里的每一步都要有界（见 STORE_STEP_TIMEOUT）：这一步卡住，租约就会过期，
    // 别的副本拿同一个已作废的 refresh token 再刷一遍
    let changed = tokio::time::timeout(
        STORE_STEP_TIMEOUT,
        okapi_store::oauth_credentials::invalidate_if_current(
            &state.pg,
            key.channel_key_id,
            key.provider,
            expected,
            code,
        ),
    )
    .await
    .map_err(|_| UpstreamError::Build("oauth_credential_write_failed".into()))?
    .map_err(|_| UpstreamError::Build("oauth_credential_write_failed".into()))?;
    if !changed {
        return replaced_credential(state, key.channel_key_id, expected).await;
    }
    super::pending::clear(state, key.channel_key_id).await;
    state.invalidate_routing_caches();
    super::health::record(state, key.channel_key_id, Some(code)).await;
    Err(UpstreamError::Status {
        status: 401,
        body: bytes::Bytes::new(),
        retry_after_secs: None,
    })
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

async fn do_refresh(
    state: &AppState,
    key: &OAuthKey<'_>,
    basis: &OAuthCredential,
) -> Result<Tokens, UpstreamError> {
    let hook = okapi_providers::registry::lookup(key.provider)
        .and_then(|adapter| adapter.account)
        .filter(|hook| hook.capabilities().refresh)
        .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".to_owned()))?;
    hook.refresh(okapi_providers::account::RefreshContext {
        http: state.upstream.http(),
        token_url: key.token_url,
        refresh_token: &basis.refresh_token,
        scope: basis.scope.as_deref(),
        proxy_url: key.proxy_url,
    })
    .await
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TINY: [Duration; 2] = [Duration::from_millis(1), Duration::from_millis(1)];

    #[tokio::test]
    async fn transient_failures_are_retried_until_one_attempt_succeeds() {
        let calls = AtomicUsize::new(0);
        let result: Result<u32, &str> = with_retries(&TINY, || async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err("db blip")
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(result, Ok(7));
        assert_eq!(calls.load(Ordering::SeqCst), 3, "两次失败后第三次成功");
    }

    #[tokio::test]
    async fn gives_up_after_the_last_delay_with_the_last_error() {
        let calls = AtomicUsize::new(0);
        let result: Result<u32, usize> = with_retries(&TINY, || async {
            Err(calls.fetch_add(1, Ordering::SeqCst))
        })
        .await;
        assert_eq!(result, Err(2), "共三次尝试，返回最后一次的错误");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_first_attempt_success_does_not_retry_or_wait() {
        let calls = AtomicUsize::new(0);
        let started = std::time::Instant::now();
        let result: Result<bool, ()> = with_retries(&WRITE_RETRY_DELAYS, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })
        .await;
        assert_eq!(result, Ok(true));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < Duration::from_millis(150));
    }
}
