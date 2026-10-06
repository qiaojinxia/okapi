//! Fixed inference flow: credential resolution, common admission, registered transport.
//! Adapter objects own URL/auth/wire behavior; converters depend only on the dialect.
use super::credentials::{CredentialManager, CredentialProvider};
use super::state::AppState;
use bytes::Bytes;
use okapi_providers::inference::{Request, Response, Surface};
use okapi_providers::registry;
use okapi_providers::{Outbound, UpstreamError};
use okapi_store::ChannelCandidate;

pub fn upstream_dialect(provider: &str, model: &str) -> &'static str {
    registry::lookup(provider).map_or("opaque", |adapter| adapter.dialect(model).as_str())
}
pub fn chat_only(provider: &str) -> bool {
    registry::lookup(provider).is_some_and(registry::ProviderDescriptor::chat_only)
}
impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub async fn infer_via(
        &self,
        cand: &ChannelCandidate,
        surface: Surface,
        base: &str,
        model: &str,
        body: Bytes,
        stream: bool,
        outbound: &Outbound,
    ) -> Result<Response, UpstreamError> {
        let endpoint = match surface {
            Surface::Chat => "/v1/chat/completions",
            Surface::Messages => "/v1/messages",
            Surface::Generate => "generateContent",
            Surface::Responses { compact: true } => "/v1/responses/compact",
            Surface::Responses { compact: false } => "/v1/responses",
        };
        super::account_control::execute(self, cand, model, endpoint, async {
            let credential = CredentialManager.resolve(self, cand).await?;
            let account_id = credential
                .oauth()
                .and_then(|credential| credential.account_id.as_deref());
            self.inference
                .infer(
                    &cand.provider,
                    Request {
                        surface,
                        base,
                        model,
                        credential: credential.material(),
                        account_id,
                        region: cand.aws_region.as_deref(),
                        api_version: cand.api_version.as_deref(),
                        body,
                        stream,
                        outbound,
                    },
                )
                .await
        })
        .await
    }
    pub async fn responses_via(
        &self,
        cand: &ChannelCandidate,
        base: &str,
        body: Bytes,
        stream: bool,
        compact: bool,
        outbound: &Outbound,
    ) -> Result<okapi_providers::ChatResponse, UpstreamError> {
        match self
            .infer_via(
                cand,
                Surface::Responses { compact },
                base,
                "",
                body,
                stream,
                outbound,
            )
            .await?
        {
            Response::OpenAi(response) => Ok(response),
            _ => Err(UpstreamError::Build("adapter_response_mismatch".into())),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn messages_via(
        &self,
        cand: &ChannelCandidate,
        base: &str,
        model: &str,
        body: Bytes,
        stream: bool,
        outbound: &Outbound,
    ) -> Result<okapi_providers::anthropic::MessagesResponse, UpstreamError> {
        match self
            .infer_via(cand, Surface::Messages, base, model, body, stream, outbound)
            .await?
        {
            Response::Messages(response) => Ok(response),
            _ => Err(UpstreamError::Build("adapter_response_mismatch".into())),
        }
    }
    pub async fn generate_via(
        &self,
        cand: &ChannelCandidate,
        base: &str,
        model: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<okapi_providers::gemini::GeminiResponse, UpstreamError> {
        match self
            .infer_via(
                cand,
                Surface::Generate,
                base,
                model,
                body,
                stream,
                &super::openai_dialect::outbound(cand),
            )
            .await?
        {
            Response::Gemini(response) => Ok(response),
            _ => Err(UpstreamError::Build("adapter_response_mismatch".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_by_provider_and_model() {
        assert_eq!(upstream_dialect("openai", "gpt-4o"), "openai");
        assert_eq!(upstream_dialect("openai_compat", "x"), "openai");
        assert_eq!(upstream_dialect("azure", "x"), "openai");
        assert_eq!(upstream_dialect("anthropic", "claude-3"), "anthropic");
        assert_eq!(
            upstream_dialect("bedrock", "us.anthropic.claude-v1:0"),
            "anthropic"
        );
        assert_eq!(upstream_dialect("gemini", "gemini-2.5-pro"), "gemini");
        assert_eq!(
            upstream_dialect("vertex", "claude-sonnet-4-5@20250929"),
            "anthropic"
        );
        assert_eq!(upstream_dialect("vertex", "gemini-2.5-flash"), "gemini");
        assert!(chat_only("bedrock") && chat_only("vertex") && !chat_only("azure"));
    }
}
