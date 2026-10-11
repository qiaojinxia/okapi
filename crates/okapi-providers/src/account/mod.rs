//! Optional provider hooks for account lifecycle. The gateway owns locks, pacing and
//! persistence; a hook owns upstream wire formats and returns normalized observations.
pub mod quota;

#[cfg(test)]
mod tests;

use crate::{
    HttpPool, Outbound, UpstreamError,
    oauth::{Pkce, Tokens},
};
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Capabilities {
    pub quota: bool,
    pub refresh: bool,
    pub subscription: Option<SubscriptionControls>,
    pub authorization: Option<AuthorizationControls>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeFormat {
    CodeState,
    CallbackUrl,
}

/// Public UI/import rules. The core validates credentials without naming a provider.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct AuthorizationControls {
    pub code_format: CodeFormat,
    pub access_token_prefix: Option<&'static str>,
    pub account_id_required: bool,
    pub import_profile: Option<crate::profiles::ClientProfile>,
}

/// No Debug: some upstreams use the PKCE verifier as the returned state.
pub struct Authorization {
    pub url: String,
    pub state: String,
    pub redirect_uri: &'static str,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuotaScope {
    Session,
    Total,
}

/// Configuration metadata belongs to the account plugin, not to the control panel.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct SubscriptionControls {
    pub quota_scope: QuotaScope,
    pub window_secs: Option<i64>,
    /// Independently configurable upstream windows, expressed in seconds.
    pub quota_windows: &'static [i64],
}

// Contexts deliberately have no Debug implementation: they carry secrets.
pub struct QuotaContext<'a> {
    pub http: &'a HttpPool,
    pub api_base: &'a str,
    pub access_token: &'a str,
    pub account_id: Option<&'a str>,
    pub outbound: &'a Outbound,
}

pub struct RefreshContext<'a> {
    pub http: &'a HttpPool,
    pub token_url: Option<&'a str>,
    pub refresh_token: &'a str,
    pub proxy_url: Option<&'a str>,
}

pub struct ExchangeContext<'a> {
    pub http: &'a HttpPool,
    pub token_url: Option<&'a str>,
    pub pasted_code: &'a str,
    pub verifier: &'a str,
    pub proxy_url: Option<&'a str>,
}

/// Register hooks on a ProviderDescriptor. Adding a provider's account behavior
/// requires no new branches in admission, scheduling or credential coordination.
pub trait AccountHooks: std::fmt::Debug + Send + Sync {
    fn capabilities(&self) -> Capabilities;

    fn authorize(&self, _pkce: &Pkce, _nonce: &str) -> Option<Authorization> {
        None
    }

    fn exchange<'a>(
        &'a self,
        _context: ExchangeContext<'a>,
    ) -> BoxFuture<'a, Result<Tokens, UpstreamError>> {
        Box::pin(async {
            Err(UpstreamError::Build(
                "oauth_authorization_unsupported".into(),
            ))
        })
    }

    /// Reapply provider semantics to cached observations from an older binary.
    fn scope_quota(&self, _snapshot: &mut quota::Snapshot) {}

    fn parse_quota(&self, _data: &Value, _now: i64) -> Option<quota::Snapshot> {
        None
    }

    fn quota<'a>(
        &'a self,
        _context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<quota::Snapshot, UpstreamError>> {
        Box::pin(async { Err(UpstreamError::Build("quota_unsupported".into())) })
    }

    fn refresh<'a>(
        &'a self,
        _context: RefreshContext<'a>,
    ) -> BoxFuture<'a, Result<Tokens, UpstreamError>> {
        Box::pin(async { Err(UpstreamError::Build("oauth_credential_expected".into())) })
    }

    /// 订阅档位（如 `pro` / `max_5x` / `max_20x`），只供控制台展示；`None` = 不支持或认不出。
    /// 与额度探测共用凭证、出口与上游地址。
    fn plan<'a>(
        &'a self,
        _context: QuotaContext<'a>,
    ) -> BoxFuture<'a, Result<Option<String>, UpstreamError>> {
        Box::pin(async { Ok(None) })
    }
}
