//! Claude Code 2.1.290 wire profile (macOS arm64), from isolated first-party captures of
//! the installed CLI: interactive main/auxiliary requests and SDK/print requests, recorded
//! through a local TLS responder with a fake token. Only the latest client is kept.
//! Authentication and account/session scheduling remain outside this module.
use super::{
    ClaudeCodeEntrypoint as Entrypoint, ClaudeCodeRequestClass as RequestClass, PreparedRequest,
    header, is_client_header, valid_uuid,
};
use crate::{
    Outbound, UpstreamError,
    profiles::identity::{self, MimicIdentity},
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::fmt::Write as _;

const VERSION: &str = super::ClaudeCodeRevision::V2_1_290.version();
const SDK_IDENTITY: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";
const HARNESS: &str =
    "You are an interactive agent that helps users with software engineering tasks.";
#[path = "cch.rs"]
mod cch;
const MAIN_BETAS: &[&str] = &[
    "claude-code-20250219",
    "interleaved-thinking-2025-05-14",
    "thinking-token-count-2026-05-13",
    "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05",
    "mid-conversation-system-2026-04-07",
    "per-turn-control-2026-07-01",
    "mid-conversation-tool-changes-2026-07-01",
    "mid-conversation-system-clear-at-2026-08-21",
    "effort-2025-11-24",
    "dangerous-tool-use-2026-09-03",
    "thinking-binding-controls-2026-08-01",
    "afk-mode-2026-01-31",
    "extended-cache-ttl-2025-04-11",
    "cache-diagnosis-2026-04-07",
];
const AUXILIARY_BETAS: &[&str] = &[
    "interleaved-thinking-2025-05-14",
    "redact-thinking-2026-02-12",
    "thinking-token-count-2026-05-13",
    "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05",
    "structured-outputs-2025-12-15",
    "cache-diagnosis-2026-04-07",
];

/// Cost the profile adds before admission can see the reshaped body:
/// - attribution + identity + harness system blocks, measured at about 70 Claude tokens;
/// - 1h cache markers on the harness and the newest content, as the official client does;
/// - Claude's tokenizer counted 4680 tokens where the local o200k estimate gave 3040
///   on English text (×1.54); reserve ×1.6.
pub(super) const ADMISSION_HINTS: super::AdmissionHints = super::AdmissionHints {
    added_prompt_tokens: 128,
    prompt_cache_write_1h: true,
    prompt_scale_permille: 1600,
};

const ADVANCED_TOOL_USE: &str = "advanced-tool-use-2025-11-20";

/// Tool search requests: the CLI's ToolSearch tool, or tool references in the conversation.
fn uses_tool_search(body: &Value) -> bool {
    fn references(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.get("type").and_then(Value::as_str) == Some("tool_reference")
                    || map.values().any(references)
            }
            Value::Array(items) => items.iter().any(references),
            _ => false,
        }
    }
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some("ToolSearch"))
        })
        || body.get("messages").is_some_and(references)
}

fn random_uuid() -> Result<String, UpstreamError> {
    let mut bytes = [0; 16];
    aws_lc_rs::rand::fill(&mut bytes)
        .map_err(|_| UpstreamError::Build("request_identity_random_failed".into()))?;
    Ok(identity::uuid_from_bytes(&bytes))
}

fn cache_markers(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(map.contains_key("cache_control"))
                + map.values().map(cache_markers).sum::<usize>()
        }
        Value::Array(items) => items.iter().map(cache_markers).sum(),
        _ => 0,
    }
}

fn has_short_cache(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("cache_control")
                .is_some_and(|cache| cache.get("ttl").and_then(Value::as_str) != Some("1h"))
                || map.values().any(has_short_cache)
        }
        Value::Array(items) => items.iter().any(has_short_cache),
        _ => false,
    }
}

fn cache_last_content(value: &mut Value, available: &mut usize, cache: &Value) {
    if *available == 0 {
        return;
    }
    if let Some(items) = value.as_array_mut()
        && let Some(last) = items.last_mut()
        && last.is_object()
        && last.get("cache_control").is_none()
        && !matches!(
            last.get("type").and_then(Value::as_str),
            Some("thinking" | "redacted_thinking")
        )
    {
        last["cache_control"] = cache.clone();
        *available -= 1;
    }
}

fn defaults(body: &mut Value, entrypoint: Entrypoint, class: RequestClass, stream: bool) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    let sonnet = obj.get("model").and_then(Value::as_str) == Some("claude-sonnet-5-5");
    let haiku = obj.get("model").and_then(Value::as_str) == Some("claude-haiku-4-5-20251001");
    if sonnet && class == RequestClass::Main {
        obj.entry("max_tokens").or_insert(json!(128_000));
        obj.entry("thinking").or_insert(json!({"type":"adaptive","display": if entrypoint == Entrypoint::Cli {"updates"} else {"omitted"}}));
        if obj
            .get("thinking")
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            == Some("adaptive")
        {
            if let Some(output) = obj
                .entry("output_config")
                .or_insert_with(|| json!({}))
                .as_object_mut()
            {
                output.entry("effort").or_insert(json!("medium"));
            }
            obj.entry("context_management")
                .or_insert(json!({"edits":[{"type":"clear_thinking_20251015","keep":"all"}]}));
        }
    } else if haiku && class == RequestClass::Auxiliary {
        obj.entry("max_tokens").or_insert(json!(32_000));
        obj.entry("thinking").or_insert(json!({"type":"disabled"}));
        obj.entry("temperature").or_insert(json!(1));
    }
    if class == RequestClass::Main {
        obj.entry("diagnostics")
            .or_insert(json!({"previous_message_id":null}));
    }
    obj.insert("stream".into(), json!(stream));
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn prepare(
    body: &[u8],
    stream: bool,
    counting: bool,
    outbound: &Outbound,
    identity: &MimicIdentity,
    entrypoint: Entrypoint,
    class: RequestClass,
) -> Result<PreparedRequest, UpstreamError> {
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    if !value.is_object() {
        return Err(UpstreamError::Build("body_not_object".into()));
    }
    let context = &outbound.context;
    let session = header(context, "x-claude-code-session-id").filter(|id| valid_uuid(id));
    // Keep the legacy conversation seed for callers without an explicit session ID.
    let uid = identity.metadata_user_id_for_session(&first_user_text(&value), session);
    let metadata: Value =
        serde_json::from_str(&uid).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let session_id = metadata["session_id"]
        .as_str()
        .ok_or_else(|| UpstreamError::Build("request_session_missing".into()))?;
    let prompt_id = match header(context, "x-claude-code-prompt-id").filter(|id| valid_uuid(id)) {
        Some(id) => id.to_owned(),
        None => random_uuid()?,
    };
    let entry = if entrypoint == Entrypoint::Cli {
        "cli"
    } else {
        "sdk-cli"
    };
    let mut billing = format!(
        "{} cc_version={VERSION}.{}; cc_entrypoint={entry}; cch=00000;",
        identity::BILLING_PREFIX,
        identity::cc_fingerprint(body, VERSION)
    );
    if class == RequestClass::Main {
        let messages = value.get("messages").and_then(Value::as_array);
        let prompts = messages
            .map_or(1, |items| {
                items
                    .iter()
                    .filter(|m| {
                        m.get("role").and_then(Value::as_str) == Some("user")
                            && !first_content_text(m).is_empty()
                    })
                    .count()
            })
            .max(1);
        let turns = messages.map_or(1, |items| {
            1 + items
                .iter()
                .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
                .count()
        });
        let origin = if entrypoint == Entrypoint::Cli {
            "human"
        } else {
            "sdk"
        };
        let index = if entrypoint == Entrypoint::Cli {
            prompts
        } else {
            prompts - 1
        };
        write!(billing, " cc_prompt_id={prompt_id}; cc_turn_origin={origin}; cc_prompt_index={index}; cc_turn_index={turns};")
            .map_err(|e| UpstreamError::Build(e.to_string()))?;
    }
    let mut system = match value.as_object_mut().and_then(|v| v.remove("system")) {
        Some(Value::String(s)) if !s.is_empty() => vec![json!({"type":"text","text":s})],
        Some(Value::Array(items)) => items,
        _ => vec![],
    };
    system.retain(|v| {
        !v.get("text").and_then(Value::as_str).is_some_and(|s| {
            s.starts_with(identity::BILLING_PREFIX)
                || s == super::super::oauth::anthropic_max::SYSTEM_PREFIX
                || s == SDK_IDENTITY
                || s == HARNESS
        })
    });
    let prefix = if entrypoint == Entrypoint::Cli {
        super::super::oauth::anthropic_max::SYSTEM_PREFIX
    } else {
        SDK_IDENTITY
    };
    let mut prepared_system = vec![
        json!({"type":"text","text":billing}),
        json!({"type":"text","text":prefix}),
    ];
    // Tools precede system/messages in the cache prefix. Do not add 1h markers to
    // mixed/5m layouts: a new shorter marker before an existing 1h marker is also invalid.
    let cache = json!({"type":"ephemeral","ttl":"1h"});
    let existing_markers = cache_markers(&value) + system.iter().map(cache_markers).sum::<usize>();
    let mut remaining = if has_short_cache(&value) || system.iter().any(has_short_cache) {
        0
    } else {
        4usize.saturating_sub(existing_markers)
    };
    if class == RequestClass::Main {
        let mut harness = json!({"type":"text","text":HARNESS});
        if remaining > 0 {
            harness["cache_control"] = cache.clone();
            harness["cache_control"]["scope"] = json!("global");
            remaining -= 1;
        }
        prepared_system.push(harness);
        let mut user_system = Value::Array(system);
        cache_last_content(&mut user_system, &mut remaining, &cache);
        prepared_system.extend(user_system.as_array().cloned().unwrap_or_default());
    } else {
        prepared_system.extend(system);
    }
    value["system"] = Value::Array(prepared_system);
    let obj = value
        .as_object_mut()
        .ok_or_else(|| UpstreamError::Build("body_not_object".into()))?;
    obj.entry("metadata")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| UpstreamError::Build("metadata_not_object".into()))?
        .insert("user_id".into(), json!(uid));
    if !counting {
        defaults(&mut value, entrypoint, class, stream);
        if class == RequestClass::Main {
            if let Some(last) = value
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .and_then(|m| m.last_mut())
            {
                if let Some(text) = last
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    last["content"] = json!([{"type":"text","text":text}]);
                }
                if let Some(content) = last.get_mut("content") {
                    cache_last_content(content, &mut remaining, &cache);
                }
            }
            if let Some(tools) = value.get_mut("tools") {
                cache_last_content(tools, &mut remaining, &cache);
            }
        }
    }
    // Sign the bytes that will actually be sent, after all profile transformations.
    // The credential adapter only adds headers; it must not reserialize this body.
    let mut wire = serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))?;
    cch::sign(&mut wire)?;
    let body = Bytes::from(wire);
    let mut betas = if class == RequestClass::Main {
        MAIN_BETAS
    } else {
        AUXILIARY_BETAS
    }
    .to_vec();
    if class == RequestClass::Main && uses_tool_search(&value) {
        // The CLI announces tool search only when its ToolSearch tool is enabled.
        let at = betas
            .iter()
            .position(|beta| *beta == "mid-conversation-tool-changes-2026-07-01")
            .map_or(betas.len(), |index| index + 1);
        betas.insert(at, ADVANCED_TOOL_USE);
    }
    if class == RequestClass::Main && entrypoint == Entrypoint::Cli {
        let at = betas
            .iter()
            .position(|beta| *beta == "thinking-binding-controls-2026-08-01")
            .map_or(betas.len(), |index| index + 1);
        betas.insert(at, "thinking-display-updates-2026-08-18");
    }
    if counting {
        betas.push(identity::BETA_TOKEN_COUNTING);
    }
    let mut output = outbound.clone();
    output.extra_headers.retain(|(name, _)| {
        !is_client_header(&name.to_ascii_lowercase())
            && !identity::FORGED_HEADER_KEYS.contains(&name.to_ascii_lowercase().as_str())
    });
    output
        .extra_headers
        .push(("anthropic-beta".into(), betas.join(",")));
    let mut headers = vec![
        (
            "user-agent".into(),
            format!("claude-cli/{VERSION} (external, {entry})"),
        ),
        ("accept".into(), "application/json".into()),
        ("x-app".into(), "cli".into()),
        ("x-stainless-lang".into(), "js".into()),
        ("x-stainless-package-version".into(), "0.128.0".into()),
        ("x-stainless-os".into(), "MacOS".into()),
        ("x-stainless-arch".into(), "arm64".into()),
        ("x-stainless-runtime".into(), "node".into()),
        ("x-stainless-runtime-version".into(), "v26.3.0".into()),
        ("x-stainless-retry-count".into(), "0".into()),
        ("x-stainless-timeout".into(), "600".into()),
        (
            "anthropic-dangerous-direct-browser-access".into(),
            "true".into(),
        ),
        ("x-claude-code-session-id".into(), session_id.to_owned()),
        ("x-claude-code-prompt-id".into(), prompt_id),
        (
            "x-claude-code-request-class".into(),
            if class == RequestClass::Main {
                "main"
            } else {
                "auxiliary"
            }
            .into(),
        ),
        ("x-client-request-id".into(), random_uuid()?),
    ];
    if stream && !counting {
        // 对 2.1.290 抓包的有意偏离：真实 SDK 仅流式 helper 带此头，防御上游校验"流式必带"。
        headers.push(("x-stainless-helper-method".into(), "stream".into()));
    }
    Ok(PreparedRequest {
        body,
        outbound: output,
        headers,
        client_betas_complete: true,
        shaped: true,
    })
}

fn first_content_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .find_map(|b| b.get("text").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned(),
        _ => String::new(),
    }
}

fn first_user_text(value: &Value) -> String {
    value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|m| {
            m.iter()
                .find(|v| v.get("role").and_then(Value::as_str) == Some("user"))
        })
        .map(first_content_text)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "claude_code_tests.rs"]
mod tests;
