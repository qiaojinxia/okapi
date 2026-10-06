//! Registered inference transport plugins. The gateway resolves credentials and
//! admission once, then invokes this interface without matching provider names.
use crate::{Outbound, UpstreamError};
use bytes::Bytes;
use futures::future::BoxFuture;
use std::{collections::HashMap, sync::Arc};
mod builtin;
pub use builtin::{anthropic, azure, bedrock, claude, codex, gemini, openai, vertex};

#[derive(Clone, Copy)]
pub enum Surface {
    Chat,
    Messages,
    Generate,
    Responses { compact: bool },
}
pub struct Request<'a> {
    pub surface: Surface,
    pub base: &'a str,
    pub model: &'a str,
    pub credential: &'a str,
    pub account_id: Option<&'a str>,
    pub region: Option<&'a str>,
    pub api_version: Option<&'a str>,
    pub body: Bytes,
    pub stream: bool,
    pub outbound: &'a Outbound,
}
pub enum Response {
    OpenAi(crate::ChatResponse),
    Messages(crate::anthropic::MessagesResponse),
    Gemini(crate::gemini::GeminiResponse),
}
pub trait Transport: Send + Sync {
    fn infer<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Response, UpstreamError>>;
}
// Resource construction is owned by the provider package. New transports can
// use the shared HTTP pool or keep private state in their registered object.
pub struct Resources {
    pub openai: crate::OpenAiUpstream,
    pub anthropic: crate::AnthropicUpstream,
    pub gemini: crate::GeminiUpstream,
    pub bedrock: crate::BedrockUpstream,
    pub vertex: crate::VertexUpstream,
}
impl Resources {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            openai: crate::OpenAiUpstream::new()?,
            anthropic: crate::AnthropicUpstream::new()?,
            gemini: crate::GeminiUpstream::new()?,
            bedrock: crate::BedrockUpstream::new()?,
            vertex: crate::VertexUpstream::new()?,
        })
    }
}
pub type Factory = fn(&Resources) -> Arc<dyn Transport>;
#[derive(Clone)]
pub struct Registry {
    transports: Arc<HashMap<&'static str, Arc<dyn Transport>>>,
}
impl Registry {
    pub fn builtin(resources: &Resources) -> Result<Self, UpstreamError> {
        Self::from_registrations(resources, crate::registry::BUILT_INS)
    }
    pub fn from_registrations(
        resources: &Resources,
        registrations: &[crate::registry::ProviderDescriptor],
    ) -> Result<Self, UpstreamError> {
        let mut transports = HashMap::new();
        for registration in registrations {
            if let Some(factory) = registration.inference
                && transports
                    .insert(registration.id, factory(resources))
                    .is_some()
            {
                return Err(UpstreamError::Build(
                    "duplicate_inference_registration".into(),
                ));
            }
        }
        Ok(Self {
            transports: Arc::new(transports),
        })
    }
    pub async fn infer(
        &self,
        provider: &str,
        request: Request<'_>,
    ) -> Result<Response, UpstreamError> {
        self.transports
            .get(provider)
            .ok_or_else(|| UpstreamError::Build("adapter_unregistered".into()))?
            .infer(request)
            .await
    }
}
