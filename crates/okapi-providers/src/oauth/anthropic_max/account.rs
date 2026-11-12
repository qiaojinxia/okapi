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
                    native_only: false,
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
            scoped: scoped_windows(data),
            threshold_window: Some("five_hour".into()),
            plan: None,
        })
    }

    fn quota<'a>(
        &'a self,
        context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<quota::Snapshot, UpstreamError>> {
        Box::pin(async move {
            let user_agent = crate::profiles::ClaudeCodeRevision::default().account_user_agent();
            let authorization = format!("Bearer {}", context.access_token);
            let response = super::send_account(
                context.http,
                reqwest::Method::GET,
                quota::url(&context, "/api/oauth/usage")?,
                &[
                    ("authorization", authorization.as_str()),
                    ("anthropic-beta", super::OAUTH_BETA),
                    ("user-agent", user_agent.as_str()),
                ],
                None,
                std::time::Duration::from_secs(10),
                context.outbound.proxy_url.as_deref(),
            )
            .await
            .map_err(|error| match error {
                UpstreamError::Timeout
                | UpstreamError::Unreachable {
                    timed_out: true, ..
                } => UpstreamError::Timeout,
                _ => UpstreamError::Connect("quota_probe".into()),
            })?;
            let data = quota::read(response).await?;
            self.parse_quota(&data, chrono::Utc::now().timestamp())
                .ok_or_else(|| UpstreamError::Build("quota_unknown".into()))
        })
    }

    fn plan<'a>(
        &'a self,
        context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, UpstreamError>> {
        Box::pin(async move {
            // 与 CLI 登录后取 profile 同形（2.1.296 源码）：axios GET，UA/Authorization/
            // Content-Type/Cache-Control 四个头，不带 beta。
            let user_agent = crate::profiles::ClaudeCodeRevision::default().account_user_agent();
            let authorization = format!("Bearer {}", context.access_token);
            let response = super::send_account(
                context.http,
                reqwest::Method::GET,
                quota::url(&context, "/api/oauth/profile")?,
                &[
                    ("user-agent", user_agent.as_str()),
                    ("authorization", authorization.as_str()),
                    ("content-type", "application/json"),
                    ("cache-control", "no-cache"),
                ],
                None,
                std::time::Duration::from_secs(10),
                context.outbound.proxy_url.as_deref(),
            )
            .await?;
            let data = quota::read(response).await?;
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
            context.scope,
            context.proxy_url,
        ))
    }
}

const WEEK_SECS: i64 = 604_800;

/// 用量应答的 `limits[]` 里 `kind == "weekly_scoped"` 的项是按模型分的周窗口（`scope.model.display_name`，已用百分比在 `percent`），
/// 真机 /usage 里的「Current week (Fable)」就是它（2.1.321 二进制）。只收名字带 fable 的，名字只许字母数字与 ` ._-`：
/// 它只用来展示，不进 `windows`（那会让一个模型族用尽就把整个账号摘掉）。
fn scoped_windows(data: &Value) -> Vec<quota::ScopedWindow> {
    data.get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("kind").and_then(Value::as_str) == Some("weekly_scoped"))
        .filter_map(|item| {
            let model = item.pointer("/scope/model/display_name")?.as_str()?.trim();
            let plain = model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'));
            if !plain || model.len() > 40 || !model.to_ascii_lowercase().contains("fable") {
                return None;
            }
            Some(quota::ScopedWindow {
                model: model.to_owned(),
                used_percent: item.get("percent").and_then(quota::percentage)?,
                resets_at: item.get("resets_at").and_then(reset_epoch),
                window_secs: Some(WEEK_SECS),
            })
        })
        .collect()
}

/// 重置时刻：Unix 秒（数字，可能带小数）或 RFC 3339 字符串（CLI 两种都认）。
fn reset_epoch(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_number() {
        // 整数部分按文本取，不走浮点转换
        let text = number.to_string();
        let whole = text
            .split_once('.')
            .map_or(text.as_str(), |(whole, _)| whole);
        return whole.parse::<i64>().ok().filter(|at| *at > 0);
    }
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|at| at.timestamp())
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
    use super::{ClaudeAccount, plan_from_profile};
    use crate::account::{AccountHooks, quota::ScopedWindow, quota::Snapshot};
    use serde_json::json;

    fn parse(data: &serde_json::Value) -> Snapshot {
        ClaudeAccount
            .parse_quota(data, 1_000)
            .expect("a snapshot with windows")
    }

    /// Claude Code 2.1.321 的 /usage：`limits[]` 里 `kind: weekly_scoped` 的项是按模型分的周窗口（真机的「Current week (Fable)」）。
    /// 它只展示：Fable 周限额用尽不能把账号的放行余量变成 0，其它模型照旧可用。
    #[test]
    fn the_fable_weekly_window_is_shown_but_never_gates_the_account() {
        let data = json!({
            "five_hour": {"utilization": 20, "resets_at": "1970-01-01T00:30:00Z"},
            "seven_day": {"utilization": 41.5, "resets_at": "1970-01-08T00:00:00Z"},
            "limits": [
                {"kind": "session", "percent": 20},
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Fable"}}, "percent": 100, "resets_at": 604_800},
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Sonnet"}}, "percent": 90, "resets_at": 604_800},
            ],
        });
        let snapshot = parse(&data);
        assert_eq!(
            snapshot
                .windows
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            ["five_hour", "seven_day"],
            "the Fable bucket is not an account window"
        );
        assert_eq!(
            snapshot.scoped,
            [ScopedWindow {
                model: "Fable".into(),
                used_percent: 100,
                resets_at: Some(604_800),
                window_secs: Some(604_800),
            }],
            "only the Fable bucket is kept (Sonnet already has its own top-level window)"
        );
        assert_eq!(
            snapshot.headroom(1_000),
            Some(80),
            "a spent Fable week must not zero the headroom"
        );
        assert_eq!(snapshot.next_reset(1_000), Some(1_800));
    }

    #[test]
    fn scoped_windows_accept_both_reset_formats_and_ignore_malformed_rows() {
        let data = json!({
            "five_hour": {"utilization": 1, "resets_at": "2030-01-01T00:00:00Z"},
            "limits": [
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Fable"}}, "percent": 37.2, "resets_at": "2030-01-08T00:00:00Z"},
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Fable Max"}}, "percent": 12, "resets_at": 1_893_456_000.0},
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Fable"}}},
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "fable<script>"}}, "percent": 3},
                {"kind": "weekly_scoped", "scope": {"model": null}, "percent": 3},
                {"kind": "weekly_scoped", "percent": 3},
                "junk",
                null,
            ],
        });
        let scoped = parse(&data).scoped;
        assert_eq!(
            scoped,
            [
                ScopedWindow {
                    model: "Fable".into(),
                    used_percent: 38,
                    resets_at: Some(1_894_060_800),
                    window_secs: Some(604_800),
                },
                ScopedWindow {
                    model: "Fable Max".into(),
                    used_percent: 12,
                    resets_at: Some(1_893_456_000),
                    window_secs: Some(604_800),
                },
            ],
            "a fraction rounds up like the account windows, a malformed row is skipped"
        );
    }

    #[test]
    fn a_snapshot_without_scoped_windows_keeps_its_old_shape() {
        let snapshot = parse(&json!({"five_hour": {"utilization": 1}}));
        assert_eq!(snapshot.scoped, vec![]);
        let stored = serde_json::to_value(&snapshot).unwrap();
        assert!(
            stored.get("scoped").is_none(),
            "an empty list is not written: {stored}"
        );
        // 旧记录（没有这个字段）照样读得回来
        let old: Snapshot =
            serde_json::from_value(json!({"observed_at": 1, "allowed": null, "windows": []}))
                .unwrap();
        assert_eq!(old.scoped, vec![]);
    }

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
