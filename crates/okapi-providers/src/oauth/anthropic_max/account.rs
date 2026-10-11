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

    fn plan<'a>(
        &'a self,
        context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, UpstreamError>> {
        Box::pin(async move {
            let user_agent = crate::profiles::ClaudeCodeRevision::default().account_user_agent();
            let data = quota::fetch(
                &context,
                "/api/oauth/profile",
                &[("user-agent", user_agent.as_str())],
            )
            .await?;
            Ok(plan_from_profile(&data))
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

/// `/api/oauth/profile` → 档位。CLI 把 `organization_type` 映射成 pro / max / team / enterprise，
/// Max 的 5x / 20x 只体现在 `rate_limit_tier`（`default_claude_max_5x` / `default_claude_max_20x`）。
#[must_use]
pub fn plan_from_profile(profile: &Value) -> Option<String> {
    let organization = profile.get("organization")?;
    let kind = organization.get("organization_type")?.as_str()?;
    let tier = organization
        .get("rate_limit_tier")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plan = match kind {
        "claude_pro" => "pro",
        "claude_max" if tier.ends_with("_20x") => "max_20x",
        "claude_max" if tier.ends_with("_5x") => "max_5x",
        "claude_max" => "max",
        "claude_team" => "team",
        "claude_enterprise" => "enterprise",
        _ => return None,
    };
    Some(plan.to_owned())
}

#[cfg(test)]
mod tests {
    use super::plan_from_profile;
    use serde_json::json;

    #[test]
    fn plan_follows_organization_type_and_rate_limit_tier() {
        let plan = |kind: &str, tier: &str| {
            plan_from_profile(
                &json!({"organization":{"organization_type":kind,"rate_limit_tier":tier}}),
            )
        };
        // Pro 取自真实账号的 profile 响应；Max 两档的 tier 串取自 2.1.296 CLI 源码
        assert_eq!(
            plan("claude_pro", "default_claude_ai").as_deref(),
            Some("pro")
        );
        assert_eq!(
            plan("claude_max", "default_claude_max_5x").as_deref(),
            Some("max_5x")
        );
        assert_eq!(
            plan("claude_max", "default_claude_max_20x").as_deref(),
            Some("max_20x")
        );
        assert_eq!(plan("claude_max", "").as_deref(), Some("max"));
        assert_eq!(plan("claude_team", "x").as_deref(), Some("team"));
        assert_eq!(plan("api", "x"), None);
        assert_eq!(plan_from_profile(&json!({})), None);
    }
}
