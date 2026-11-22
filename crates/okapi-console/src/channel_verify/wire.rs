//! 通过进程内 `DataPlaneBridge` + `ChannelPin` 发起真实数据面调用，
//! 统一解析 OpenAI Chat、OpenAI Responses 与 Anthropic Messages 的流式 SSE 或 JSON 响应。

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, header};
use okapi_engine::channel_test::ChannelPin;
use okapi_engine::state::AppState;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Surface {
    Chat,
    Responses,
    Messages,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ParsedToolCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ParsedResponse {
    pub text: String,
    pub response_model: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub tool_calls: Vec<ParsedToolCall>,
    pub has_thinking_block: bool,
    pub signature: String,
}

#[derive(Clone)]
pub(super) struct CallContext {
    pub state: AppState,
    pub headers: HeaderMap,
    pub pin: ChannelPin,
}

impl CallContext {
    pub(super) fn new(state: AppState, mut headers: HeaderMap, pin: ChannelPin) -> Self {
        headers.remove(crate::playground::SELECTED_KEY_HEADER);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream, application/json"),
        );
        Self {
            state,
            headers,
            pin,
        }
    }
}

pub(super) async fn invoke_pinned(
    ctx: &CallContext,
    surface: Surface,
    body: &Value,
) -> Result<ParsedResponse, String> {
    let payload = serde_json::to_vec(body).map_err(|e| format!("encode request: {e}"))?;
    let state = ctx.state.clone();
    let headers = ctx.headers.clone();
    let pin = ctx.pin.clone();
    let trace = okapi_engine::diagnostics::Trace::new(&headers);
    let bytes = Bytes::from(payload);

    let call = async move {
        let Ok(bridge) = state.data_plane() else {
            return Err("data plane unavailable".to_owned());
        };
        let call_state = state.clone();
        let resp = match surface {
            Surface::Chat => bridge.chat_completions(call_state, headers, bytes).await,
            Surface::Responses => bridge.responses(call_state, headers, bytes).await,
            Surface::Messages => bridge.messages(call_state, headers, bytes).await,
        };
        Ok(resp)
    };
    let response = tokio::time::timeout(
        CALL_TIMEOUT,
        trace.scope(okapi_engine::channel_test::scope(pin, Box::pin(call))),
    )
    .await
    .map_err(|_| "upstream request timed out (90s)".to_owned())??;

    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let raw = tokio::time::timeout(
        CALL_TIMEOUT,
        axum::body::to_bytes(response.into_body(), MAX_RESPONSE_BYTES),
    )
    .await
    .map_err(|_| "reading upstream body timed out (90s)".to_owned())?
    .map_err(|e| format!("read upstream response body: {e}"))?;

    if !status.is_success() {
        return Err(format_http_error(status.as_u16(), &raw));
    }

    if content_type.contains("text/event-stream") || looks_like_sse(&raw) {
        Ok(parse_sse_response(surface, &raw))
    } else {
        parse_json_response(surface, &raw)
    }
}

fn looks_like_sse(raw: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(raw) else {
        return false;
    };
    let trimmed = text.trim_start();
    trimmed.starts_with("data:") || trimmed.starts_with("event:")
}

fn format_http_error(status: u16, raw: &[u8]) -> String {
    if let Ok(json) = serde_json::from_slice::<Value>(raw) {
        let msg = json
            .pointer("/error/message")
            .or_else(|| json.pointer("/error/code"))
            .or_else(|| json.get("message"))
            .and_then(Value::as_str);
        if let Some(msg) = msg
            && !msg.trim().is_empty()
        {
            return format!("HTTP {status}: {}", truncate(msg.trim(), 240));
        }
    }
    let plain = String::from_utf8_lossy(raw);
    let snippet = truncate(plain.trim(), 200);
    if snippet.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {snippet}")
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

pub(super) fn parse_sse_response(surface: Surface, raw: &[u8]) -> ParsedResponse {
    let text = String::from_utf8_lossy(raw);
    let mut out = ParsedResponse::default();
    let mut tool_slots: BTreeMap<usize, ParsedToolCall> = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let Some(data) = trimmed.strip_prefix("data:") else {
            continue;
        };
        let payload = data.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        match surface {
            Surface::Chat => apply_chat_sse(&value, &mut out, &mut tool_slots),
            Surface::Responses => apply_responses_sse(&value, &mut out, &mut tool_slots),
            Surface::Messages => apply_messages_sse(&value, &mut out, &mut tool_slots),
        }
    }
    if out.tool_calls.is_empty() && !tool_slots.is_empty() {
        out.tool_calls = tool_slots.into_values().collect();
    }
    out
}

fn apply_chat_sse(
    v: &Value,
    out: &mut ParsedResponse,
    slots: &mut BTreeMap<usize, ParsedToolCall>,
) {
    if let Some(model) = v.get("model").and_then(Value::as_str)
        && !model.is_empty()
    {
        out.response_model = Some(model.to_owned());
    }
    if let Some(pt) = v.pointer("/usage/prompt_tokens").and_then(Value::as_i64) {
        out.prompt_tokens = Some(pt);
    }
    let Some(delta) = v.pointer("/choices/0/delta") else {
        return;
    };
    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        out.text.push_str(content);
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for (pos, call) in calls.iter().enumerate() {
            let idx = call
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(pos);
            let slot = slots.entry(idx).or_default();
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                slot.name.push_str(name);
            }
            if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                slot.arguments.push_str(args);
            }
        }
    }
}

fn apply_responses_sse(
    v: &Value,
    out: &mut ParsedResponse,
    slots: &mut BTreeMap<usize, ParsedToolCall>,
) {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "response.created" | "response.in_progress" | "response.completed"
        | "response.incomplete" => {
            if let Some(model) = v.pointer("/response/model").and_then(Value::as_str)
                && !model.is_empty()
            {
                out.response_model = Some(model.to_owned());
            }
            if let Some(it) = v
                .pointer("/response/usage/input_tokens")
                .and_then(Value::as_i64)
            {
                out.prompt_tokens = Some(it);
            }
            if kind == "response.completed"
                && let Some(items) = v.pointer("/response/output").and_then(Value::as_array)
            {
                extract_responses_output_items(items, out, slots);
            }
        }
        "response.output_text.delta" => {
            if let Some(delta) = v.get("delta").and_then(Value::as_str) {
                out.text.push_str(delta);
            }
        }
        "response.output_item.added" | "response.output_item.done" => {
            let idx = v
                .get("output_index")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0);
            if v.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                let slot = slots.entry(idx).or_default();
                if let Some(name) = v.pointer("/item/name").and_then(Value::as_str) {
                    name.clone_into(&mut slot.name);
                }
                if let Some(args) = v.pointer("/item/arguments").and_then(Value::as_str)
                    && !args.is_empty()
                {
                    args.clone_into(&mut slot.arguments);
                }
            }
        }
        "response.function_call_arguments.delta" => {
            let idx = v
                .get("output_index")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0);
            if let Some(delta) = v.get("delta").and_then(Value::as_str) {
                slots.entry(idx).or_default().arguments.push_str(delta);
            }
        }
        "response.function_call_arguments.done" => {
            let idx = v
                .get("output_index")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0);
            let slot = slots.entry(idx).or_default();
            if let Some(name) = v.get("name").and_then(Value::as_str)
                && !name.is_empty()
            {
                name.clone_into(&mut slot.name);
            }
            if let Some(args) = v.get("arguments").and_then(Value::as_str) {
                args.clone_into(&mut slot.arguments);
            }
        }
        _ => {}
    }
}

fn extract_responses_output_items(
    items: &[Value],
    out: &mut ParsedResponse,
    slots: &mut BTreeMap<usize, ParsedToolCall>,
) {
    let mut fallback_text = String::new();
    for (idx, item) in items.iter().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let slot = slots.entry(idx).or_default();
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    name.clone_into(&mut slot.name);
                }
                if let Some(args) = item.get("arguments").and_then(Value::as_str) {
                    args.clone_into(&mut slot.arguments);
                }
            }
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(t) = part.get("text").and_then(Value::as_str) {
                            fallback_text.push_str(t);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if out.text.is_empty() && !fallback_text.is_empty() {
        out.text = fallback_text;
    }
}

fn anthropic_total_input_tokens(usage: &Value) -> Option<i64> {
    let base = usage.get("input_tokens").and_then(Value::as_i64);
    let cache_create = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    base.map(|b| b + cache_create + cache_read)
}

fn apply_messages_sse(
    v: &Value,
    out: &mut ParsedResponse,
    slots: &mut BTreeMap<usize, ParsedToolCall>,
) {
    match v.get("type").and_then(Value::as_str).unwrap_or_default() {
        "message_start" => {
            if let Some(model) = v.pointer("/message/model").and_then(Value::as_str)
                && !model.is_empty()
            {
                out.response_model = Some(model.to_owned());
            }
            if let Some(usage) = v.pointer("/message/usage")
                && let Some(total) = anthropic_total_input_tokens(usage)
            {
                out.prompt_tokens = Some(total);
            }
        }
        "content_block_start" => {
            let idx = v
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0);
            let Some(block) = v.get("content_block") else {
                return;
            };
            match block.get("type").and_then(Value::as_str) {
                Some("thinking" | "redacted_thinking") => {
                    out.has_thinking_block = true;
                    if let Some(sig) = block.get("signature").and_then(Value::as_str) {
                        out.signature.push_str(sig);
                    }
                }
                Some("tool_use") => {
                    let slot = slots.entry(idx).or_default();
                    if let Some(name) = block.get("name").and_then(Value::as_str) {
                        name.clone_into(&mut slot.name);
                    }
                    if let Some(input) = block.get("input")
                        && input.as_object().is_some_and(|m| !m.is_empty())
                        && let Ok(s) = serde_json::to_string(input)
                    {
                        slot.arguments = s;
                    }
                }
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        out.text.push_str(t);
                    }
                }
                _ => {}
            }
        }
        "content_block_delta" => {
            let idx = v
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0);
            let Some(delta) = v.get("delta") else {
                return;
            };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(t) = delta.get("text").and_then(Value::as_str) {
                        out.text.push_str(t);
                    }
                }
                Some("thinking_delta") => {
                    out.has_thinking_block = true;
                }
                Some("signature_delta") => {
                    out.has_thinking_block = true;
                    if let Some(sig) = delta.get("signature").and_then(Value::as_str) {
                        out.signature.push_str(sig);
                    }
                }
                Some("input_json_delta") => {
                    if let Some(part) = delta.get("partial_json").and_then(Value::as_str) {
                        slots.entry(idx).or_default().arguments.push_str(part);
                    }
                }
                _ => {}
            }
        }
        "message_delta" => {
            if out.prompt_tokens.is_none()
                && let Some(usage) = v.get("usage")
                && let Some(total) = anthropic_total_input_tokens(usage)
            {
                out.prompt_tokens = Some(total);
            }
        }
        _ => {}
    }
}

pub(super) fn parse_json_response(surface: Surface, raw: &[u8]) -> Result<ParsedResponse, String> {
    let v: Value =
        serde_json::from_slice(raw).map_err(|e| format!("parse upstream JSON response: {e}"))?;
    let mut out = ParsedResponse::default();
    if let Some(model) = v.get("model").and_then(Value::as_str)
        && !model.is_empty()
    {
        out.response_model = Some(model.to_owned());
    }
    match surface {
        Surface::Chat => {
            out.prompt_tokens = v.pointer("/usage/prompt_tokens").and_then(Value::as_i64);
            if let Some(msg) = v.pointer("/choices/0/message") {
                if let Some(content) = msg.get("content").and_then(Value::as_str) {
                    out.text.push_str(content);
                }
                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let arguments = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        out.tool_calls.push(ParsedToolCall { name, arguments });
                    }
                }
            }
        }
        Surface::Responses => {
            out.prompt_tokens = v.pointer("/usage/input_tokens").and_then(Value::as_i64);
            let mut slots = BTreeMap::new();
            if let Some(items) = v.get("output").and_then(Value::as_array) {
                extract_responses_output_items(items, &mut out, &mut slots);
            }
            out.tool_calls = slots.into_values().collect();
        }
        Surface::Messages => {
            if let Some(usage) = v.get("usage") {
                out.prompt_tokens = anthropic_total_input_tokens(usage);
            }
            if let Some(blocks) = v.get("content").and_then(Value::as_array) {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                out.text.push_str(t);
                            }
                        }
                        Some("thinking" | "redacted_thinking") => {
                            out.has_thinking_block = true;
                            if let Some(sig) = block.get("signature").and_then(Value::as_str) {
                                out.signature.push_str(sig);
                            }
                        }
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            let arguments = block
                                .get("input")
                                .and_then(|v| serde_json::to_string(v).ok())
                                .unwrap_or_default();
                            out.tool_calls.push(ParsedToolCall { name, arguments });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anthropic_messages_sse_with_cached_usage_and_signature() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":12,\"cache_creation_input_tokens\":100,\"cache_read_input_tokens\":50}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"EqoBCkYIARgC\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"K3JzaWduYXR1cmU=\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"391\"}}\n\n",
        );
        let parsed = parse_sse_response(Surface::Messages, sse.as_bytes());
        assert_eq!(parsed.response_model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(parsed.prompt_tokens, Some(162));
        assert!(parsed.has_thinking_block);
        assert_eq!(parsed.signature, "EqoBCkYIARgCK3JzaWduYXR1cmU=");
        assert_eq!(parsed.text, "391");
    }

    #[test]
    fn parses_openai_chat_sse_tool_call_and_usage() {
        let sse = concat!(
            "data: {\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"verify_echo\",\"arguments\":\"{\\\"nonce\\\":\\\"\"}}]}}]}\n\n",
            "data: {\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"okapi-1234\\\"}\"}}]}}]}\n\n",
            "data: {\"model\":\"gpt-5.4\",\"choices\":[],\"usage\":{\"prompt_tokens\":47}}\n\n",
            "data: [DONE]\n\n",
        );
        let parsed = parse_sse_response(Surface::Chat, sse.as_bytes());
        assert_eq!(parsed.response_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(parsed.prompt_tokens, Some(47));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "verify_echo");
        assert_eq!(parsed.tool_calls[0].arguments, "{\"nonce\":\"okapi-1234\"}");
    }

    #[test]
    fn parses_responses_sse_function_call_and_text() {
        let sse = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-5.4-codex\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"name\":\"verify_echo\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"nonce\\\":\\\"abc\\\"}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-5.4-codex\",\"usage\":{\"input_tokens\":31}}}\n\n",
        );
        let parsed = parse_sse_response(Surface::Responses, sse.as_bytes());
        assert_eq!(parsed.response_model.as_deref(), Some("gpt-5.4-codex"));
        assert_eq!(parsed.prompt_tokens, Some(31));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "verify_echo");
        assert_eq!(parsed.tool_calls[0].arguments, "{\"nonce\":\"abc\"}");
    }
}
