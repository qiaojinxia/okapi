//! Shared chat endpoint compatibility for routing, diagnostics and connection examples.
use okapi_store::ChannelCandidate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum Ingress {
    #[default]
    #[serde(rename = "chat_completions")]
    OpenAi,
    #[serde(rename = "messages")]
    Anthropic,
    #[serde(rename = "responses")]
    Responses,
    #[serde(rename = "responses_compact")]
    ResponsesCompact,
    #[serde(rename = "gemini")]
    Gemini,
}

impl Ingress {
    pub const ALL: [Self; 5] = [
        Self::OpenAi,
        Self::Responses,
        Self::Anthropic,
        Self::Gemini,
        Self::ResponsesCompact,
    ];

    pub fn endpoint(self) -> &'static str {
        match self {
            Self::OpenAi => "/v1/chat/completions",
            Self::Anthropic => "/v1/messages",
            Self::Responses => "/v1/responses",
            Self::ResponsesCompact => "/v1/responses/compact",
            Self::Gemini => "/v1beta/models/{model}:generateContent",
        }
    }

    pub fn supports(
        self,
        provider: &str,
        upstream_model: &str,
        native: bool,
        caps: &Value,
    ) -> bool {
        if provider == "codex" && !matches!(self, Self::Responses | Self::ResponsesCompact) {
            return false;
        }
        match self {
            Self::Anthropic => {
                super::dialect::upstream_dialect(provider, upstream_model) != "gemini"
            }
            Self::ResponsesCompact => {
                native && caps.get("compact").and_then(Value::as_bool) != Some(false)
            }
            _ => true,
        }
    }

    pub fn accepts(self, channel: &ChannelCandidate, model: &str) -> bool {
        self.supports(
            &channel.provider,
            channel.upstream_model(model),
            channel.responses_native,
            &channel.capabilities,
        )
    }

    /// Only suggest endpoints reachable through the caller's current pool and active keys.
    pub fn available_endpoints(channels: &[ChannelCandidate], model: &str) -> Vec<&'static str> {
        Self::ALL
            .into_iter()
            .filter(|ingress| channels.iter().any(|c| ingress.accepts(c, model)))
            .map(Self::endpoint)
            .collect()
    }
}
