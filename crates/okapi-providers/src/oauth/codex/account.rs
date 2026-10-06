//! Codex account wire behavior, including upstream-defined quota window durations.
use crate::account::{
    AccountHooks, Authorization, AuthorizationControls, Capabilities, CodeFormat, ExchangeContext,
    QuotaContext, QuotaScope, RefreshContext, SubscriptionControls, quota,
};
use crate::{UpstreamError, oauth::Tokens};
use futures::future::BoxFuture;
use serde_json::Value;

#[derive(Debug)]
pub struct CodexAccount;
pub static HOOKS: CodexAccount = CodexAccount;

impl AccountHooks for CodexAccount {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            quota: true,
            refresh: true,
            authorization: Some(AuthorizationControls {
                code_format: CodeFormat::CallbackUrl,
                access_token_prefix: None,
                account_id_required: true,
                import_profile: None,
            }),
            subscription: Some(SubscriptionControls {
                quota_scope: QuotaScope::Total,
                window_secs: None,
                quota_windows: &[18_000, 604_800],
            }),
        }
    }

    fn authorize(&self, pkce: &crate::oauth::Pkce, nonce: &str) -> Option<Authorization> {
        Some(Authorization {
            url: super::authorize_url(pkce, nonce),
            state: nonce.to_owned(),
            redirect_uri: super::REDIRECT_URI,
        })
    }

    fn exchange<'a>(
        &'a self,
        context: ExchangeContext<'a>,
    ) -> BoxFuture<'a, Result<Tokens, UpstreamError>> {
        Box::pin(async move {
            let (code, _) = super::split_pasted_code(context.pasted_code);
            super::exchange(
                context.http,
                context.token_url.unwrap_or(super::TOKEN_URL),
                &code,
                context.verifier,
                context.proxy_url,
            )
            .await
        })
    }

    fn scope_quota(&self, snapshot: &mut quota::Snapshot) {
        // Prefer the longest reported allowance. With unknown durations the
        // secondary allowance is the provider's long-term window, if present.
        snapshot.threshold_window = snapshot
            .windows
            .iter()
            .max_by_key(|window| {
                (
                    window.window_secs.unwrap_or(0),
                    window.name == "secondary_window",
                )
            })
            .map(|window| window.name.clone());
    }

    fn parse_quota(&self, data: &Value, now: i64) -> Option<quota::Snapshot> {
        let root = data.get("rate_limit").or_else(|| data.get("rate_limits"))?;
        let allowed = root.get("allowed").and_then(Value::as_bool);
        let windows = ["primary_window", "secondary_window"]
            .into_iter()
            .filter_map(|name| {
                let value = root.get(name)?;
                Some(quota::Window {
                    name: name.into(),
                    used_percent: value.get("used_percent").and_then(quota::percentage)?,
                    window_secs: value
                        .get("limit_window_seconds")
                        .and_then(Value::as_i64)
                        .filter(|n| *n > 0),
                    resets_at: value.get("reset_at").and_then(Value::as_i64).or_else(|| {
                        value
                            .get("reset_after_seconds")
                            .and_then(Value::as_i64)
                            .filter(|n| *n >= 0)
                            .map(|n| now.saturating_add(n))
                    }),
                })
            })
            .collect::<Vec<_>>();
        let mut snapshot = quota::Snapshot {
            observed_at: now,
            allowed,
            windows,
            threshold_window: None,
        };
        self.scope_quota(&mut snapshot);
        (!snapshot.windows.is_empty() || allowed == Some(false)).then_some(snapshot)
    }

    fn quota<'a>(
        &'a self,
        context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<quota::Snapshot, UpstreamError>> {
        Box::pin(async move {
            let base = reqwest::Url::parse(context.api_base)
                .map_err(|_| UpstreamError::Build("quota_api_base".into()))?;
            let path = if base.path().starts_with("/backend-api/") {
                "/backend-api/wham/usage"
            } else {
                "/api/codex/usage"
            };
            let mut headers = vec![("user-agent", "codex-cli")];
            if let Some(account) = context.account_id {
                headers.push(("chatgpt-account-id", account));
            }
            let data = quota::fetch(&context, path, &headers).await?;
            self.parse_quota(&data, chrono::Utc::now().timestamp())
                .ok_or_else(|| UpstreamError::Build("quota_unknown".into()))
        })
    }

    fn refresh<'a>(
        &'a self,
        context: RefreshContext<'a>,
    ) -> BoxFuture<'a, Result<Tokens, UpstreamError>> {
        Box::pin(super::refresh(
            context.http,
            context.token_url.unwrap_or(super::TOKEN_URL),
            context.refresh_token,
            context.proxy_url,
        ))
    }
}
