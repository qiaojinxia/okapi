//! Adapter-owned request extensions, after protocol conversion and before transport.
//! No credential refresh, scheduling, ledger or storage access is available here.
use crate::{Outbound, UpstreamError};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod claude_code;
pub mod identity;

#[derive(Clone, Debug, Default)]
pub struct RequestContext {
    pub extensions: Value,
    pub identity_seed: Option<String>,
    pub client_headers: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMode {
    #[default]
    Auto,
    Passthrough,
    Mimic,
}

/// A revision selects UA, SDK/runtime, betas and body rules together. Only the latest
/// captured client is implemented; configurations saved for retired revisions read as it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum ClaudeCodeRevision {
    #[default]
    #[serde(rename = "2.1.290", alias = "2.1.286", alias = "2.1.258")]
    V2_1_290,
}

impl ClaudeCodeRevision {
    pub const fn version(self) -> &'static str {
        match self {
            Self::V2_1_290 => "2.1.290",
        }
    }

    /// User-Agent the client sends to its account APIs (`/api/...`), as captured;
    /// Messages requests use `claude-cli/<version> (external, <entrypoint>)` instead.
    pub fn account_user_agent(self) -> String {
        format!("claude-code/{}", self.version())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum ClaudeCodeEntrypoint {
    #[default]
    #[serde(rename = "cli")]
    Cli,
    #[serde(rename = "sdk-cli")]
    SdkCli,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeCodeRequestClass {
    #[default]
    Main,
    Auxiliary,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "name", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ClientProfile {
    Native {},
    ClaudeCode {
        #[serde(default)]
        mode: ProfileMode,
        #[serde(default)]
        revision: ClaudeCodeRevision,
        #[serde(default)]
        entrypoint: ClaudeCodeEntrypoint,
        #[serde(default)]
        request_class: ClaudeCodeRequestClass,
    },
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Extensions {
    #[serde(default)]
    client_profile: Option<ClientProfile>,
}

pub fn client_profile(extensions: &Value) -> Result<Option<ClientProfile>, UpstreamError> {
    if extensions.is_null() {
        return Ok(None);
    }
    if !extensions.is_object() {
        return Err(UpstreamError::Build("request_extensions_invalid".into()));
    }
    serde_json::from_value::<Extensions>(extensions.clone())
        .map(|value| value.client_profile)
        .map_err(|_| UpstreamError::Build("request_extensions_invalid".into()))
}

/// How a client profile changes a request's cost before it reshapes the body. Admission
/// reserves an upper bound from these hints; settlement still bills upstream usage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionHints {
    /// Prompt tokens the profile prepends (identity and attribution system blocks).
    pub added_prompt_tokens: u32,
    /// The profile marks the prompt for 1h prompt caching: an uncached prompt is
    /// billed as 1h cache writes on its first request.
    pub prompt_cache_write_1h: bool,
    /// Local tokenization uses an OpenAI vocabulary; scale it (per mille) to bound
    /// the upstream tokenizer's count.
    pub prompt_scale_permille: u32,
}

impl Default for AdmissionHints {
    fn default() -> Self {
        Self {
            added_prompt_tokens: 0,
            prompt_cache_write_1h: false,
            prompt_scale_permille: 1000,
        }
    }
}

impl AdmissionHints {
    /// The most expensive of two channels' hints, for a reservation any of them may serve.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        Self {
            added_prompt_tokens: self.added_prompt_tokens.max(other.added_prompt_tokens),
            prompt_cache_write_1h: self.prompt_cache_write_1h || other.prompt_cache_write_1h,
            prompt_scale_permille: self.prompt_scale_permille.max(other.prompt_scale_permille),
        }
    }

    /// Prompt tokens to reserve for `estimated` locally counted tokens.
    #[must_use]
    pub fn prompt_tokens(self, estimated: u32) -> u32 {
        let scaled = u64::from(estimated)
            .saturating_mul(u64::from(self.prompt_scale_permille))
            .div_ceil(1000);
        u32::try_from(scaled)
            .unwrap_or(u32::MAX)
            .saturating_add(self.added_prompt_tokens)
    }
}

/// Admission hints for a channel's configured extension. Invalid or absent extensions
/// change nothing: those requests reach the upstream unmodified.
#[must_use]
pub fn admission_hints(extensions: &Value) -> AdmissionHints {
    match client_profile(extensions) {
        Ok(Some(ClientProfile::ClaudeCode { mode, .. })) if mode != ProfileMode::Passthrough => {
            claude_code::ADMISSION_HINTS
        }
        _ => AdmissionHints::default(),
    }
}

pub fn validate_extensions(provider: &str, extensions: &Value) -> Result<(), UpstreamError> {
    let profile = client_profile(extensions)?;
    let adapter = crate::registry::lookup(provider)
        .ok_or_else(|| UpstreamError::Build("adapter_unregistered".into()))?;
    if matches!(profile, Some(ClientProfile::ClaudeCode { .. }))
        && !adapter
            .extensions
            .contains(&crate::registry::RequestExtensionKind::ClaudeCode)
    {
        return Err(UpstreamError::Build("request_extension_unsupported".into()));
    }
    Ok(())
}

pub fn is_client_header(name: &str) -> bool {
    matches!(
        name,
        "user-agent"
            | "accept-language"
            | "x-app"
            | "anthropic-beta"
            | "anthropic-dangerous-direct-browser-access"
            | "x-claude-code-session-id"
            | "x-claude-code-prompt-id"
            | "x-claude-code-request-class"
            | "x-client-request-id"
            | "originator"
            | "version"
            | "session_id"
            | "conversation_id"
            | "openai-beta"
            | "x-codex-beta-features"
            | "x-codex-installation-id"
            | "x-codex-window-id"
    ) || name.starts_with("x-stainless-")
        || name.starts_with("x-codex-turn-")
}

fn header<'a>(context: &'a RequestContext, name: &str) -> Option<&'a str> {
    context
        .client_headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn legacy_metadata(value: &str) -> bool {
    value
        .strip_prefix("user_")
        .and_then(|value| value.split_once("_account_"))
        .is_some_and(|(device, tail)| {
            device.len() == 64
                && device.bytes().all(|byte| byte.is_ascii_hexdigit())
                && tail
                    .split_once("_session_")
                    .is_some_and(|(_, session)| valid_uuid(session))
        })
}

/// Compatibility classification protects native system/cache bytes, never grants authority.
fn native_claude_code(body: &[u8], context: &RequestContext) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(user_id) = value.pointer("/metadata/user_id").and_then(Value::as_str) else {
        return false;
    };
    let valid_id = serde_json::from_str::<Value>(user_id)
        .ok()
        .is_some_and(|id| {
            id.get("device_id")
                .and_then(Value::as_str)
                .is_some_and(|device| {
                    device.len() == 64 && device.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                && id
                    .get("session_id")
                    .and_then(Value::as_str)
                    .is_some_and(valid_uuid)
        })
        || legacy_metadata(user_id);
    if !valid_id {
        return false;
    }
    header(context, "user-agent").is_some_and(|ua| ua.starts_with("claude-cli/"))
        || value
            .get("system")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.starts_with(identity::BILLING_PREFIX))
                })
            })
}

pub struct PreparedRequest {
    pub body: Bytes,
    pub outbound: Outbound,
    /// Authentication belongs to the adapter, never the request extension.
    pub headers: Vec<(String, String)>,
    /// The extension supplied the complete feature set. Credential adapters add only
    /// their authentication feature; they do not know client names or revisions.
    pub client_betas_complete: bool,
    /// A configured extension owns the request shape (including `native`, which turns
    /// shaping off): credential adapters must not apply their own default body rules.
    pub shaped: bool,
}

pub fn prepare_anthropic(
    body: Bytes,
    stream: bool,
    counting: bool,
    outbound: &Outbound,
    account_id: Option<&str>,
) -> Result<PreparedRequest, UpstreamError> {
    let profile = client_profile(&outbound.context.extensions)?;
    let mut prepared = PreparedRequest {
        body,
        outbound: outbound.clone(),
        headers: Vec::new(),
        client_betas_complete: false,
        shaped: profile.is_some(),
    };
    let Some(ClientProfile::ClaudeCode {
        mode,
        revision,
        entrypoint,
        request_class,
    }) = profile
    else {
        return Ok(prepared);
    };
    if mode == ProfileMode::Passthrough
        || mode == ProfileMode::Auto && native_claude_code(&prepared.body, &outbound.context)
    {
        prepared.client_betas_complete = true;
        for (name, value) in &outbound.context.client_headers {
            if is_client_header(name) {
                prepared
                    .outbound
                    .extra_headers
                    .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
                prepared
                    .outbound
                    .extra_headers
                    .push((name.clone(), value.clone()));
            }
        }
        return Ok(prepared);
    }
    // The seed is the channel key; the account comes from the credential. Both are stable,
    // so one key keeps one simulated installation across requests and replicas.
    let seed = outbound
        .context
        .identity_seed
        .as_deref()
        .ok_or_else(|| UpstreamError::Build("request_identity_context_required".into()))?;
    let identity = identity::MimicIdentity::from_seed(seed, account_id, revision.version());
    claude_code::prepare(
        &prepared.body,
        stream,
        counting,
        &prepared.outbound,
        &identity,
        entrypoint,
        request_class,
    )
}

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod tests;
