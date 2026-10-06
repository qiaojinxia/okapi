//! Each built-in owns only its transport/protocol behavior.
use super::{Factory, Request, Resources, Response, Surface, Transport};
use crate::UpstreamError;
use futures::future::BoxFuture;
use std::sync::Arc;
fn unsupported() -> UpstreamError {
    UpstreamError::Build("unsupported_endpoint".into())
}
fn required_base<'a>(request: &Request<'a>) -> Result<&'a str, UpstreamError> {
    (!request.base.trim().is_empty())
        .then_some(request.base)
        .ok_or_else(|| UpstreamError::Build("api_base_missing".into()))
}
macro_rules! plugin {
    ($factory:ident, $name:ident, $client:ty, $make:expr, $this:ident, $request:ident, $body:block) => {
        struct $name($client);
        impl Transport for $name {
            fn infer<'a>(&'a self, $request: Request<'a>) -> BoxFuture<'a,Result<Response,UpstreamError>> {
                let $this = &self.0;
                Box::pin(async move $body)
            }
        }
        pub fn $factory(resources: &Resources) -> Arc<dyn Transport> { Arc::new($name(($make)(resources))) }
    }
}
plugin!(
    openai,
    OpenAi,
    crate::OpenAiUpstream,
    |r: &Resources| r.openai.clone(),
    client,
    r,
    {
        let response = match r.surface {
            Surface::Chat => {
                client
                    .chat(r.base, r.credential, r.body, r.stream, r.outbound)
                    .await?
            }
            Surface::Responses { compact: true } => {
                client
                    .responses_compact(r.base, r.credential, r.body, r.outbound)
                    .await?
            }
            Surface::Responses { compact: false } => {
                client
                    .responses(r.base, r.credential, r.body, r.stream, r.outbound)
                    .await?
            }
            _ => return Err(unsupported()),
        };
        Ok(Response::OpenAi(response))
    }
);
plugin!(
    azure,
    Azure,
    crate::AzureUpstream,
    |r: &Resources| crate::AzureUpstream::new(r.openai.clone()),
    client,
    r,
    {
        if !matches!(r.surface, Surface::Chat) {
            return Err(unsupported());
        }
        let response = client
            .chat(
                required_base(&r)?,
                r.api_version.unwrap_or(crate::azure::DEFAULT_API_VERSION),
                r.model,
                r.credential,
                r.body,
                r.stream,
                r.outbound,
            )
            .await?;
        Ok(Response::OpenAi(response))
    }
);
plugin!(
    anthropic,
    Anthropic,
    crate::AnthropicUpstream,
    |r: &Resources| r.anthropic.clone(),
    client,
    r,
    {
        if !matches!(r.surface, Surface::Messages) {
            return Err(unsupported());
        }
        Ok(Response::Messages(
            client
                .messages(r.base, r.credential, r.body, r.stream, r.outbound)
                .await?,
        ))
    }
);
plugin!(
    gemini,
    Gemini,
    crate::GeminiUpstream,
    |r: &Resources| r.gemini.clone(),
    client,
    r,
    {
        if !matches!(r.surface, Surface::Generate) {
            return Err(unsupported());
        }
        Ok(Response::Gemini(
            client
                .generate(r.base, r.credential, r.model, r.body, r.stream, r.outbound)
                .await?,
        ))
    }
);
plugin!(
    bedrock,
    Bedrock,
    crate::BedrockUpstream,
    |r: &Resources| r.bedrock.clone(),
    client,
    r,
    {
        if !matches!(r.surface, Surface::Messages) {
            return Err(unsupported());
        }
        Ok(Response::Messages(
            client
                .messages(
                    required_base(&r)?,
                    r.region,
                    r.credential,
                    r.model,
                    r.body,
                    r.stream,
                    r.outbound,
                )
                .await?,
        ))
    }
);
plugin!(
    vertex,
    Vertex,
    crate::VertexUpstream,
    |r: &Resources| r.vertex.clone(),
    client,
    r,
    {
        let base = required_base(&r)?;
        match r.surface {
            Surface::Messages => Ok(Response::Messages(
                client
                    .messages(base, r.credential, r.model, r.body, r.stream, r.outbound)
                    .await?,
            )),
            Surface::Generate => Ok(Response::Gemini(
                client
                    .generate(base, r.credential, r.model, r.body, r.stream, r.outbound)
                    .await?,
            )),
            _ => Err(unsupported()),
        }
    }
);
plugin!(
    claude,
    Claude,
    crate::AnthropicUpstream,
    |r: &Resources| r.anthropic.clone(),
    client,
    r,
    {
        if !matches!(r.surface, Surface::Messages) {
            return Err(unsupported());
        }
        Ok(Response::Messages(
            crate::oauth::anthropic_max::messages(
                client.http(),
                r.base,
                r.credential,
                r.body,
                r.stream,
                r.outbound,
                r.account_id,
            )
            .await?,
        ))
    }
);
plugin!(
    codex,
    Codex,
    crate::OpenAiUpstream,
    |r: &Resources| r.openai.clone(),
    client,
    r,
    {
        let response = match r.surface {
            Surface::Responses { compact: true } => {
                crate::oauth::codex::responses_compact(
                    client.http(),
                    r.base,
                    r.credential,
                    r.account_id,
                    r.body,
                    r.outbound,
                )
                .await?
            }
            Surface::Responses { compact: false } => {
                crate::oauth::codex::responses(
                    client.http(),
                    r.base,
                    r.credential,
                    r.account_id,
                    r.body,
                    r.stream,
                    r.outbound,
                )
                .await?
            }
            _ => return Err(unsupported()),
        };
        Ok(Response::OpenAi(response))
    }
);
// Assert factory signatures stay uniform without requiring changes to the gateway.
const _: Factory = vertex;
