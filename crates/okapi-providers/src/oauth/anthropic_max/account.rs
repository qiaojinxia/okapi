//! Claude account wire behavior. No scheduling, persistence or locking lives here.
use crate::account::{
    AccountHooks, Authorization, AuthorizationControls, Capabilities, CodeFormat, ExchangeContext,
    QuotaContext, QuotaScope, RefreshContext, SubscriptionControls, quota,
};
use crate::{UpstreamError, oauth::Tokens};
use futures::future::BoxFuture;
use serde_json::Value;

#[derive(Debug)]
pub struct ClaudeAccount;
pub static HOOKS: ClaudeAccount = ClaudeAccount;

impl AccountHooks for ClaudeAccount {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            quota: true,
            refresh: true,
            authorization: Some(AuthorizationControls {
                code_format: CodeFormat::CodeState,
                access_token_prefix: Some("sk-ant-oat"),
                account_id_required: false,
                import_profile: Some(crate::profiles::ClientProfile::ClaudeCode {
                    mode: crate::profiles::ProfileMode::Mimic,
                    revision: crate::profiles::ClaudeCodeRevision::V2_1_290,
                    entrypoint: crate::profiles::ClaudeCodeEntrypoint::Cli,
                    request_class: crate::profiles::ClaudeCodeRequestClass::Main,
                }),
            }),
            subscription: Some(SubscriptionControls {
                quota_scope: QuotaScope::Session,
                window_secs: Some(18_000),
                quota_windows: &[18_000, 604_800],
            }),
        }
    }

    fn authorize(&self, pkce: &crate::oauth::Pkce, _nonce: &str) -> Option<Authorization> {
        Some(Authorization {
            url: super::authorize_url(pkce),
            state: pkce.verifier.clone(),
            redirect_uri: super::REDIRECT_URI,
        })
    }

    fn exchange<'a>(
        &'a self,
        context: ExchangeContext<'a>,
    ) -> BoxFuture<'a, Result<Tokens, UpstreamError>> {
        let (code, _) = super::split_pasted_code(context.pasted_code);
        Box::pin(super::exchange(
            context.http,
            context.token_url.unwrap_or(super::TOKEN_URL),
            code,
            context.verifier,
            context.proxy_url,
        ))
    }

    fn scope_quota(&self, snapshot: &mut quota::Snapshot) {
        snapshot.threshold_window = Some("five_hour".into());
    }

    fn parse_quota(&self, data: &Value, now: i64) -> Option<quota::Snapshot> {
        let windows = [("five_hour", 18_000), ("seven_day", 604_800)]
            .into_iter()
            .filter_map(|(name, seconds)| {
                let value = data.get(name)?;
                Some(quota::Window {
                    name: name.into(),
                    used_percent: value.get("utilization").and_then(quota::percentage)?,
                    resets_at: value
                        .get("resets_at")
                        .and_then(Value::as_str)
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|at| at.timestamp()),
                    window_secs: Some(seconds),
                })
            })
            .collect::<Vec<_>>();
        (!windows.is_empty()).then_some(quota::Snapshot {
            observed_at: now,
            allowed: None,
            windows,
            threshold_window: Some("five_hour".into()),
        })
    }

    fn quota<'a>(
        &'a self,
        context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<quota::Snapshot, UpstreamError>> {
        Box::pin(async move {
            let user_agent = crate::profiles::ClaudeCodeRevision::default().account_user_agent();
            let data = quota::fetch(
                &context,
                "/api/oauth/usage",
                &[
                    ("anthropic-beta", "oauth-2025-04-20"),
                    ("user-agent", user_agent.as_str()),
                ],
            )
            .await?;
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
