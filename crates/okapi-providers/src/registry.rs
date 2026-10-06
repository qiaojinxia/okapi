//! Compiled adapter registrations. Protocol, credential and transport are separate axes.
//! The registry contains no storage handles, secrets or normalized request bodies.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    OpenAi,
    OpenAiCompatible,
    Azure,
    Anthropic,
    Gemini,
    Bedrock,
    Vertex,
    AnthropicOAuth,
    CodexOAuth,
    CustomPass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthKind {
    Anthropic,
    Codex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    StaticKey,
    OAuth(OAuthKind),
    CloudIdentity,
    PassThrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    OpenAi,
    Anthropic,
    Gemini,
    Opaque,
}

impl Dialect {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Opaque => "opaque",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiFormat {
    ChatCompletions,
    Messages,
    Responses,
    ResponsesCompact,
    Gemini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireTransport {
    Http,
    ResponsesWebSocket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestExtensionKind {
    ClaudeCode,
}

pub use okapi_api::provider_contract::NativeResponses;

#[derive(Clone, Copy, Debug)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub kind: AdapterKind,
    pub inference: Option<crate::inference::Factory>,
    pub dialect_for_model: fn(&str) -> Dialect,
    pub chat_only: bool,
    pub credential: CredentialKind,
    pub default_base: Option<&'static str>,
    pub native_responses: NativeResponses,
    pub ingress: &'static [ApiFormat],
    pub transports: &'static [WireTransport],
    /// Compiled extension support, independent of credential kind and transport.
    pub extensions: &'static [RequestExtensionKind],
    pub forward_client_identity: bool,
    /// Optional account behavior, independent of ingress protocol.
    pub account: Option<&'static dyn crate::account::AccountHooks>,
}

impl ProviderDescriptor {
    #[must_use]
    pub fn account_capabilities(&self) -> crate::account::Capabilities {
        self.account
            .map_or_else(crate::account::Capabilities::default, |hook| {
                hook.capabilities()
            })
    }

    #[must_use]
    pub fn dialect(&self, model: &str) -> Dialect {
        (self.dialect_for_model)(model)
    }

    #[must_use]
    pub fn accepts(&self, format: ApiFormat, model: &str, native: bool) -> bool {
        self.ingress.contains(&format)
            && !(format == ApiFormat::Messages && self.dialect(model) == Dialect::Gemini)
            && (format != ApiFormat::ResponsesCompact
                || self.native_responses.enabled(Some(native)))
    }

    #[must_use]
    pub fn supports_transport(&self, transport: WireTransport) -> bool {
        self.transports.contains(&transport)
    }

    #[must_use]
    pub const fn chat_only(&self) -> bool {
        self.chat_only
    }
}

fn openai_dialect(_: &str) -> Dialect {
    Dialect::OpenAi
}
fn anthropic_dialect(_: &str) -> Dialect {
    Dialect::Anthropic
}
fn gemini_dialect(_: &str) -> Dialect {
    Dialect::Gemini
}
fn vertex_dialect(model: &str) -> Dialect {
    if crate::vertex::is_anthropic_model(model) {
        Dialect::Anthropic
    } else {
        Dialect::Gemini
    }
}
fn opaque_dialect(_: &str) -> Dialect {
    Dialect::Opaque
}

const CHAT_FORMATS: &[ApiFormat] = &[
    ApiFormat::ChatCompletions,
    ApiFormat::Messages,
    ApiFormat::Responses,
    ApiFormat::ResponsesCompact,
    ApiFormat::Gemini,
];
const RESPONSES_FORMATS: &[ApiFormat] = &[ApiFormat::Responses, ApiFormat::ResponsesCompact];
const HTTP: &[WireTransport] = &[WireTransport::Http];
const HTTP_AND_WS: &[WireTransport] = &[WireTransport::Http, WireTransport::ResponsesWebSocket];

/// Adding an adapter starts with an explicit registration, rather than a fallback to OpenAI.
pub const BUILT_INS: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: okapi_api::provider_contract::OPENAI.id,
        kind: AdapterKind::OpenAi,
        inference: Some(crate::inference::openai),
        dialect_for_model: openai_dialect,
        chat_only: false,
        credential: CredentialKind::StaticKey,
        default_base: Some("https://api.openai.com/v1"),
        native_responses: okapi_api::provider_contract::OPENAI.native_responses,
        ingress: CHAT_FORMATS,
        transports: HTTP_AND_WS,
        extensions: &[],
        forward_client_identity: false,
        account: None,
    },
    ProviderDescriptor {
        id: okapi_api::provider_contract::OPENAI_COMPAT.id,
        kind: AdapterKind::OpenAiCompatible,
        inference: Some(crate::inference::openai),
        dialect_for_model: openai_dialect,
        chat_only: false,
        credential: CredentialKind::StaticKey,
        default_base: Some("https://api.openai.com/v1"),
        native_responses: okapi_api::provider_contract::OPENAI_COMPAT.native_responses,
        ingress: CHAT_FORMATS,
        // Preserve the existing WS ingress boundary; compatibility requires separate verification.
        transports: HTTP,
        extensions: &[],
        forward_client_identity: false,
        account: None,
    },
    ProviderDescriptor {
        id: "azure",
        kind: AdapterKind::Azure,
        inference: Some(crate::inference::azure),
        dialect_for_model: openai_dialect,
        chat_only: false,
        credential: CredentialKind::StaticKey,
        default_base: None,
        native_responses: NativeResponses::Unsupported,
        ingress: CHAT_FORMATS,
        transports: HTTP,
        extensions: &[],
        forward_client_identity: false,
        account: None,
    },
    ProviderDescriptor {
        id: "anthropic",
        kind: AdapterKind::Anthropic,
        inference: Some(crate::inference::anthropic),
        dialect_for_model: anthropic_dialect,
        chat_only: false,
        credential: CredentialKind::StaticKey,
        default_base: Some("https://api.anthropic.com/v1"),
        native_responses: NativeResponses::Unsupported,
        ingress: CHAT_FORMATS,
        transports: HTTP,
        extensions: &[RequestExtensionKind::ClaudeCode],
        forward_client_identity: false,
        account: None,
    },
    ProviderDescriptor {
        id: "gemini",
        kind: AdapterKind::Gemini,
        inference: Some(crate::inference::gemini),
        dialect_for_model: gemini_dialect,
        chat_only: false,
        credential: CredentialKind::StaticKey,
        default_base: Some("https://generativelanguage.googleapis.com/v1beta"),
        native_responses: NativeResponses::Unsupported,
        ingress: CHAT_FORMATS,
        transports: HTTP,
        extensions: &[],
        forward_client_identity: false,
        account: None,
    },
    ProviderDescriptor {
        id: "bedrock",
        kind: AdapterKind::Bedrock,
        inference: Some(crate::inference::bedrock),
        dialect_for_model: anthropic_dialect,
        chat_only: true,
        credential: CredentialKind::CloudIdentity,
        default_base: None,
        native_responses: NativeResponses::Unsupported,
        ingress: CHAT_FORMATS,
        transports: HTTP,
        extensions: &[],
        forward_client_identity: false,
        account: None,
    },
    ProviderDescriptor {
        id: "vertex",
        kind: AdapterKind::Vertex,
        inference: Some(crate::inference::vertex),
        dialect_for_model: vertex_dialect,
        chat_only: true,
        credential: CredentialKind::CloudIdentity,
        default_base: None,
        native_responses: NativeResponses::Unsupported,
        ingress: CHAT_FORMATS,
        transports: HTTP,
        extensions: &[],
        forward_client_identity: false,
        account: None,
    },
    ProviderDescriptor {
        id: "anthropic_max",
        kind: AdapterKind::AnthropicOAuth,
        inference: Some(crate::inference::claude),
        dialect_for_model: anthropic_dialect,
        chat_only: true,
        credential: CredentialKind::OAuth(OAuthKind::Anthropic),
        default_base: Some(crate::oauth::anthropic_max::DEFAULT_API_BASE),
        native_responses: NativeResponses::Unsupported,
        ingress: CHAT_FORMATS,
        transports: HTTP,
        extensions: &[RequestExtensionKind::ClaudeCode],
        forward_client_identity: true,
        account: Some(&crate::oauth::anthropic_max::account::HOOKS),
    },
    ProviderDescriptor {
        id: okapi_api::provider_contract::CODEX.id,
        kind: AdapterKind::CodexOAuth,
        inference: Some(crate::inference::codex),
        dialect_for_model: openai_dialect,
        chat_only: true,
        credential: CredentialKind::OAuth(OAuthKind::Codex),
        default_base: Some(crate::oauth::codex::DEFAULT_API_BASE),
        native_responses: okapi_api::provider_contract::CODEX.native_responses,
        ingress: RESPONSES_FORMATS,
        transports: HTTP_AND_WS,
        extensions: &[],
        forward_client_identity: true,
        account: Some(&crate::oauth::codex::account::HOOKS),
    },
    ProviderDescriptor {
        id: "custom_pass",
        kind: AdapterKind::CustomPass,
        inference: None,
        dialect_for_model: opaque_dialect,
        chat_only: false,
        credential: CredentialKind::PassThrough,
        default_base: None,
        native_responses: NativeResponses::Unsupported,
        ingress: &[],
        transports: HTTP,
        extensions: &[],
        forward_client_identity: false,
        account: None,
    },
];

#[must_use]
pub fn lookup(id: &str) -> Option<&'static ProviderDescriptor> {
    BUILT_INS.iter().find(|registration| registration.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrations_are_unique_and_unknown_ids_have_no_default() {
        let ids: std::collections::HashSet<_> = BUILT_INS.iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), BUILT_INS.len());
        assert!(lookup("new-unimplemented-adapter").is_none());
    }

    #[test]
    fn storage_and_executable_protocol_defaults_share_one_contract() {
        for registration in BUILT_INS {
            assert_eq!(
                registration.native_responses,
                okapi_api::provider_contract::native_responses_for(registration.id)
            );
            assert_eq!(
                registration.inference.is_some(),
                registration.id != "custom_pass"
            );
        }
    }

    #[test]
    fn account_capabilities_are_derived_from_optional_hooks() {
        #[derive(Debug)]
        struct QuotaOnly;
        impl crate::account::AccountHooks for QuotaOnly {
            fn capabilities(&self) -> crate::account::Capabilities {
                crate::account::Capabilities {
                    quota: true,
                    refresh: false,
                    ..Default::default()
                }
            }
        }
        let ordinary = lookup("openai").unwrap();
        assert_eq!(
            ordinary.account_capabilities(),
            crate::account::Capabilities::default()
        );
        let extended = ProviderDescriptor {
            account: Some(&QuotaOnly),
            ..*ordinary
        };
        assert_eq!(extended.credential, CredentialKind::StaticKey);
        assert!(extended.account_capabilities().quota);
        assert!(!extended.account_capabilities().refresh);
    }

    #[test]
    fn subscription_credentials_do_not_expand_protocol_entitlements() {
        let codex = lookup("codex").unwrap();
        assert_eq!(codex.credential, CredentialKind::OAuth(OAuthKind::Codex));
        assert!(codex.accepts(ApiFormat::Responses, "x", false));
        assert!(codex.accepts(ApiFormat::ResponsesCompact, "x", false));
        assert!(!codex.accepts(ApiFormat::ChatCompletions, "x", true));
        assert!(!codex.accepts(ApiFormat::Messages, "x", true));
        assert!(!codex.accepts(ApiFormat::Gemini, "x", true));
    }

    #[test]
    fn cloud_dialect_is_resolved_per_model_and_has_no_guessed_base() {
        let vertex = lookup("vertex").unwrap();
        assert_eq!(
            vertex.dialect("claude-sonnet-4-5@20250929"),
            Dialect::Anthropic
        );
        assert_eq!(vertex.dialect("gemini-2.5-pro"), Dialect::Gemini);
        assert!(!vertex.accepts(ApiFormat::Messages, "gemini-2.5-pro", false));
        for id in ["vertex", "bedrock", "azure"] {
            assert!(lookup(id).unwrap().default_base.is_none());
        }
    }

    #[test]
    fn native_responses_and_transport_defaults_preserve_legacy_channels() {
        assert!(lookup("openai").unwrap().native_responses.enabled(None));
        assert!(
            !lookup("openai_compat")
                .unwrap()
                .native_responses
                .enabled(None)
        );
        assert!(
            lookup("codex")
                .unwrap()
                .native_responses
                .enabled(Some(false))
        );
        for d in BUILT_INS {
            assert!(d.supports_transport(WireTransport::Http));
            assert_eq!(
                d.supports_transport(WireTransport::ResponsesWebSocket),
                matches!(d.kind, AdapterKind::OpenAi | AdapterKind::CodexOAuth)
            );
        }
    }
}
