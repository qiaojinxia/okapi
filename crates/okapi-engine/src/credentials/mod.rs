//! Credential resolution is independent of ingress protocol and transport.
//! Keep secrets out of Debug output and leave cloud signing/token derivation to its adapter.
pub mod health;
pub mod oauth;
pub mod pending;

use super::state::AppState;
use okapi_providers::UpstreamError;
use okapi_providers::registry::{self, CredentialKind};
use okapi_store::ChannelCandidate;
use okapi_store::credential::OAuthCredential;
use std::future::Future;

pub enum ResolvedCredential<'a> {
    Static(&'a str),
    OAuth(OAuthCredential),
    CloudIdentity(&'a str),
    PassThrough(&'a str),
}

impl std::fmt::Debug for ResolvedCredential<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Static(_) => "Static([redacted])",
            Self::OAuth(_) => "OAuth([redacted])",
            Self::CloudIdentity(_) => "CloudIdentity([redacted])",
            Self::PassThrough(_) => "PassThrough([redacted])",
        })
    }
}

impl ResolvedCredential<'_> {
    /// A token for static/OAuth credentials, or the original identity document for cloud adapters.
    pub fn material(&self) -> &str {
        match self {
            Self::Static(secret) | Self::CloudIdentity(secret) | Self::PassThrough(secret) => {
                secret
            }
            Self::OAuth(credential) => &credential.access_token,
        }
    }

    pub fn oauth(&self) -> Option<&OAuthCredential> {
        match self {
            Self::OAuth(credential) => Some(credential),
            Self::Static(_) | Self::CloudIdentity(_) | Self::PassThrough(_) => None,
        }
    }
}

/// Decode stored material without refreshing it. Account observation and request
/// execution share this credential boundary; provider names never decide the format.
pub fn stored_credential<'a>(
    provider: &str,
    plaintext: &'a str,
) -> Result<ResolvedCredential<'a>, UpstreamError> {
    let adapter = registry::lookup(provider)
        .ok_or_else(|| UpstreamError::Build("adapter_unregistered".into()))?;
    match adapter.credential {
        CredentialKind::StaticKey => Ok(ResolvedCredential::Static(plaintext)),
        CredentialKind::OAuth(_) => OAuthCredential::parse(plaintext)
            .map(ResolvedCredential::OAuth)
            .ok_or_else(|| UpstreamError::Build("oauth_credential_expected".into())),
        CredentialKind::CloudIdentity => Ok(ResolvedCredential::CloudIdentity(plaintext)),
        CredentialKind::PassThrough => Ok(ResolvedCredential::PassThrough(plaintext)),
    }
}

pub trait CredentialProvider {
    fn resolve<'a>(
        &'a self,
        state: &'a AppState,
        channel: &'a ChannelCandidate,
    ) -> impl Future<Output = Result<ResolvedCredential<'a>, UpstreamError>> + Send;
}

/// Both HTTP forwarding and WS handshakes use this provider. No token cache or locks are duplicated.
pub struct CredentialManager;

impl CredentialProvider for CredentialManager {
    async fn resolve<'a>(
        &'a self,
        state: &'a AppState,
        channel: &'a ChannelCandidate,
    ) -> Result<ResolvedCredential<'a>, UpstreamError> {
        let adapter = registry::lookup(&channel.provider)
            .ok_or_else(|| UpstreamError::Build("adapter_unregistered".to_owned()))?;
        match adapter.credential {
            CredentialKind::OAuth(_) => oauth::fresh_credential(state, channel)
                .await
                .map(ResolvedCredential::OAuth),
            _ => stored_credential(&channel.provider, &channel.credential),
        }
    }
}

/// 凭证里藏着的出站地址也过闸：Vertex 服务账号 JSON 的 `token_uri`——网关换 access token 时
/// 把 JWT 断言 POST 到它，测活还会把非 2xx 响应体前 300 字回给管理员。凭证密封存储，写入口是
/// 唯一能看见它的地方；按凭证形状判断而不按 provider，先以别的协议存下再 PATCH 成 vertex 也
/// 绕不过去。违规统一回 `credential_token_uri`（管理员没动 api_base，不该看到 api_base 的参数名）。
pub async fn validate_credential(
    pg: &sqlx::PgPool,
    credential: &str,
) -> Result<(), okapi_kernel::error::AppError> {
    let Some(sa) = okapi_providers::vertex::ServiceAccount::parse(credential) else {
        return Ok(());
    };
    okapi_kernel::ssrf::validate_api_base(pg, &sa.token_uri)
        .await
        .map_err(|_| {
            okapi_kernel::error::AppError::bad_request().with_param("credential_token_uri")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_never_discloses_tokens_or_cloud_private_keys() {
        let credentials = [
            ResolvedCredential::Static("secret-api-key"),
            ResolvedCredential::CloudIdentity("secret-private-key"),
            ResolvedCredential::PassThrough("secret-pass-key"),
            ResolvedCredential::OAuth(OAuthCredential {
                access_token: "secret-access".into(),
                refresh_token: "secret-refresh".into(),
                expires_at: 123,
                account_id: Some("secret-account".into()),
                account_label: None,
                scope: None,
            }),
        ];
        for credential in credentials {
            let debug = format!("{credential:?}");
            assert!(debug.contains("[redacted]"));
            assert!(!debug.contains("secret"));
        }
    }
}
