//! Bounded OAuth pre-refresh; credentials stay in the shared gateway manager.
use crate::gateway::credentials::{health, oauth};
use crate::gateway::state::AppState;
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RefreshPolicy {
    pub enabled: bool,
    pub interval_secs: u64,
    pub refresh_margin_secs: i64,
    pub batch_size: u16,
    pub concurrency: u8,
    pub requests_per_second: u8,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 30,
            refresh_margin_secs: 300,
            batch_size: 100,
            concurrency: 2,
            requests_per_second: 1,
        }
    }
}

impl RefreshPolicy {
    pub fn parse(value: &Value) -> Option<Self> {
        let policy = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(value.clone()).ok()?
        };
        ((5..=300).contains(&policy.interval_secs)
            && (120..=3600).contains(&policy.refresh_margin_secs)
            && (1..=1000).contains(&policy.batch_size)
            && (1..=8).contains(&policy.concurrency)
            && (1..=5).contains(&policy.requests_per_second))
        .then_some(policy)
    }
}

pub async fn run(state: AppState, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut after = 0;
    loop {
        if *stop.borrow() {
            return;
        }
        let setting = state.setting_cached("oauth_refresh_policy").await;
        let policy = setting
            .as_ref()
            .as_ref()
            .map_or_else(|| Some(RefreshPolicy::default()), RefreshPolicy::parse);
        let policy = policy.unwrap_or_else(|| {
            tracing::warn!("invalid oauth_refresh_policy; background refresh disabled");
            RefreshPolicy {
                enabled: false,
                ..RefreshPolicy::default()
            }
        });
        tokio::select! {
            _ = stop.changed() => return,
            result = refresh_once(&state, policy, &mut after) => {
                if let Err(error) = result {
                    tracing::warn!(error = %error, "OAuth pre-refresh scan failed");
                }
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(Duration::from_secs(policy.interval_secs)) => {}
        }
    }
}

/// Cursor persists across ticks; a full page never prevents later keys from being considered.
pub async fn refresh_once(
    state: &AppState,
    policy: RefreshPolicy,
    after: &mut i64,
) -> Result<(), okapi_store::StoreError> {
    let providers: Vec<&str> = okapi_providers::registry::BUILT_INS
        .iter()
        .filter(|adapter| adapter.account.is_some())
        .map(|adapter| adapter.id)
        .collect();
    let rows = okapi_store::oauth_credentials::scan(
        &state.pg,
        *after,
        i64::from(policy.batch_size),
        &providers,
    )
    .await?;
    *after = if rows.len() < usize::from(policy.batch_size) {
        0
    } else {
        rows.last().map_or(0, |row| row.id)
    };
    let pacer = Arc::new(tokio::sync::Mutex::new(tokio::time::Instant::now()));
    let spacing = Duration::from_millis(1000 / u64::from(policy.requests_per_second.max(1)));
    stream::iter(rows)
        .map(|row| {
            let pacer = pacer.clone();
            async move {
                let Ok(plain) = okapi_store::credential::open(
                    state.master_key.as_deref(),
                    &row.credential_ciphertext,
                ) else {
                    health::record(state, row.id, Some("oauth_credential_read_failed")).await;
                    return;
                };
                let Ok(credential) =
                    crate::gateway::credentials::stored_credential(&row.provider, &plain)
                else {
                    health::record(state, row.id, Some("oauth_credential_expected")).await;
                    return;
                };
                let now = chrono::Utc::now().timestamp();
                let Ok(control) =
                    crate::gateway::account_control::policy(state, row.channel_id).await
                else {
                    return;
                };
                let capabilities = okapi_providers::registry::lookup(&row.provider)
                    .map(okapi_providers::registry::ProviderDescriptor::account_capabilities)
                    .unwrap_or_default();
                let refresh = capabilities.refresh
                    && policy.enabled
                    && control.refresh_mode
                        == crate::gateway::account_control::policy::RefreshMode::Managed
                    && credential.oauth().is_some_and(|oauth| {
                        oauth.needs_refresh(
                            now,
                            policy.refresh_margin_secs.max(control.refresh_margin_secs),
                        )
                    })
                    && health::read(state, row.id)
                        .await
                        .is_none_or(|h| h.retry_ready(now));
                if !(refresh || capabilities.quota) {
                    return;
                }
                let deadline = {
                    let mut next = pacer.lock().await;
                    let deadline = (*next).max(tokio::time::Instant::now());
                    *next = deadline + spacing;
                    deadline
                };
                tokio::time::sleep_until(deadline).await;
                let token_url = row.settings.get("oauth_token_url").and_then(Value::as_str);
                let Some(proxy_url) = refresh_egress(state, row.id).await else {
                    return;
                };
                let key = oauth::OAuthKey {
                    channel_key_id: row.id,
                    provider: &row.provider,
                    token_url,
                    proxy_url: proxy_url.as_deref(),
                };
                if refresh
                    && let Err(error) = oauth::refresh_due(
                        state,
                        &key,
                        &plain,
                        policy.refresh_margin_secs.max(control.refresh_margin_secs),
                    )
                    .await
                {
                    tracing::warn!(key = row.id, error = %error, "OAuth pre-refresh failed");
                }
                crate::gateway::account_control::quota::poll(state, &row, &plain).await;
            }
        })
        .buffer_unordered(usize::from(policy.concurrency.max(1)))
        .collect::<Vec<_>>()
        .await;
    Ok(())
}

/// 刷新与推理同一出口（§11.41）。出口不可用（停用 / 未分配）→ None：跳过这一轮，绝不直连——
/// 订阅账号的 token 端点同样看 IP。
async fn refresh_egress(state: &AppState, key: i64) -> Option<Option<String>> {
    let resolved =
        okapi_store::egress::resolve_for_key(&state.pg, key, state.master_key.as_deref()).await;
    let Ok(Some(Ok(proxy_url))) =
        resolved.map(|egress| egress.map(okapi_store::egress::Resolved::proxy_url))
    else {
        tracing::warn!(key, "OAuth pre-refresh skipped: egress unavailable");
        return None;
    };
    Some(proxy_url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_rejects_unbounded_or_misspelled_configuration() {
        assert!(RefreshPolicy::parse(&Value::Null).unwrap().enabled);
        assert!(
            !RefreshPolicy::parse(&json!({"enabled":false}))
                .unwrap()
                .enabled
        );
        for value in [
            json!({"concurrency":0}),
            json!({"batch_size":1001}),
            json!({"requests_per_second":6}),
            json!({"interval_secs":0}),
            json!({"refresh_margin_secs":119}),
            json!({"enable":false}),
            json!(false),
        ] {
            assert!(RefreshPolicy::parse(&value).is_none(), "{value}");
        }
    }
}
