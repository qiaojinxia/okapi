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

/// `GET {base}/wham/usage`（ChatGPT 后端）或 `/api/codex/usage`：额度窗口与档位同一份响应，
/// 一次请求两样都取。
async fn usage(context: &QuotaContext<'_>) -> Result<Value, UpstreamError> {
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
    quota::fetch(context, path, &headers).await
}

/// 档位原样取上游的 snake_case 值（plus / pro / prolite / team …），展示名由控制台映射。
pub(super) fn known_plan(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|plan| !plan.is_empty() && *plan != "unknown")
        .map(str::to_owned)
}
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
            plan: known_plan(data.get("plan_type")),
        };
        self.scope_quota(&mut snapshot);
        (!snapshot.windows.is_empty() || allowed == Some(false)).then_some(snapshot)
    }

    fn quota<'a>(
        &'a self,
        context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<quota::Snapshot, UpstreamError>> {
        Box::pin(async move {
            let data = usage(&context).await?;
            self.parse_quota(&data, chrono::Utc::now().timestamp())
                .ok_or_else(|| UpstreamError::Build("quota_unknown".into()))
        })
    }

    /// 档位平时随额度应答带回（用量接口的 `plan_type`，即 Codex CLI `/status` 的来源）。
    /// 应答里没有时才走到这里：只看 access token 的 `chatgpt_plan_type` claim（签发时的档位，
    /// 最多晚一个 token 有效期），不再请求一次限流很严的用量接口。
    fn plan<'a>(
        &'a self,
        context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, UpstreamError>> {
        Box::pin(async move { Ok(super::plan_from_access_token(context.access_token)) })
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

#[cfg(test)]
mod tests {
    use super::{CodexAccount, known_plan};
    use crate::account::AccountHooks;
    use serde_json::json;

    #[test]
    fn quota_response_carries_the_plan() {
        let window =
            json!({"used_percent":10,"limit_window_seconds":18000,"reset_at":4_070_908_800_i64});
        let snapshot = CodexAccount
            .parse_quota(
                &json!({"plan_type":"prolite","rate_limit":{"primary_window":window}}),
                0,
            )
            .unwrap();
        assert_eq!(snapshot.plan.as_deref(), Some("prolite"));
        let without = CodexAccount
            .parse_quota(&json!({"rate_limit":{"primary_window":window}}), 0)
            .unwrap();
        assert_eq!(without.plan, None, "没给档位时交给 plan 钩子看 token");
    }

    #[test]
    fn plan_keeps_the_upstream_value_and_skips_placeholders() {
        // 用量接口形如 {"plan_type":"plus","rate_limit":{...}}；取值表见 Codex CLI 0.160 的 PlanType
        for plan in ["plus", "pro", "prolite", "team", "business"] {
            assert_eq!(known_plan(Some(&json!(plan))).as_deref(), Some(plan));
        }
        for missing in [json!("unknown"), json!(""), json!(null), json!(3)] {
            assert_eq!(known_plan(Some(&missing)), None, "{missing}");
        }
        assert_eq!(known_plan(None), None);
    }
}
