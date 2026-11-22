//! 对话内容存档采集器：请求/响应体清洗（剥离图片/音频 Base64 占位）、安全 UTF-8 截断、
//! 多协议流式增量拼装（OpenAI Chat / Responses / Anthropic Messages / Gemini）与异步落盘。

use crate::ingress::Ingress;
use crate::state::AppState;
use chrono::Utc;
use okapi_store::conversation_archive::ArchiveRecord;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

const BASE64_SNIFF_MIN_BYTES: usize = 256;

/// 单次请求的存档上下文：仅在 `settings.conversation_archive.enabled == true` 时构造，
/// 关闭时 `RequestBilling.archive` 为 `None`，对热路径零分配。
#[derive(Debug, Clone)]
pub struct ArchiveCapture {
    pub request_body: String,
    pub request_truncated: bool,
    pub max_body_bytes: usize,
}

/// 已采集的响应侧内容。
#[derive(Debug, Clone, Default)]
pub struct CapturedResponse {
    pub response_body: String,
    pub response_text: String,
    pub reasoning_text: String,
    pub tool_calls: String,
    pub truncated: bool,
}

/// 剥离内联 Base64 媒体数据并按字节上限截断，生成可安全落盘的请求快照。
#[must_use]
pub fn prepare_request(raw_body: &[u8], max_body_bytes: usize) -> ArchiveCapture {
    let (request_body, request_truncated) = sanitize_and_truncate_json(raw_body, max_body_bytes);
    ArchiveCapture {
        request_body,
        request_truncated,
        max_body_bytes,
    }
}

/// 按 UTF-8 字符边界截断到 `max_bytes` 以内。
#[must_use]
pub fn truncate_utf8(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    (input[..end].to_owned(), true)
}

fn push_capped(target: &mut String, chunk: &str, max_bytes: usize, truncated: &mut bool) {
    if chunk.is_empty() {
        return;
    }
    if target.len() >= max_bytes {
        *truncated = true;
        return;
    }
    let remaining = max_bytes - target.len();
    if chunk.len() <= remaining {
        target.push_str(chunk);
    } else {
        let mut end = remaining;
        while end > 0 && !chunk.is_char_boundary(end) {
            end -= 1;
        }
        target.push_str(&chunk[..end]);
        *truncated = true;
    }
}

fn base64_placeholder(mime: &str, b64_len: usize) -> String {
    let approx_kb = b64_len.saturating_mul(3) / 4 / 1024;
    if approx_kb > 0 {
        format!("[base64 {mime} omitted, ~{approx_kb} KB]")
    } else {
        format!("[base64 {mime} omitted, {b64_len} B]")
    }
}

fn redact_data_uri(s: &str) -> Option<String> {
    if s.len() < BASE64_SNIFF_MIN_BYTES || !s.starts_with("data:") {
        return None;
    }
    let (header, data) = s.split_once(";base64,")?;
    let mime = header.strip_prefix("data:").unwrap_or("binary");
    Some(base64_placeholder(mime, data.len()))
}

/// 递归剥离 OpenAI / Anthropic / Gemini / Audio 等常见内联 Base64 媒体字段。
pub fn sanitize_json_value(value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Some(placeholder) = redact_data_uri(s) {
                *s = placeholder;
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_json_value(item);
            }
        }
        Value::Object(map) => {
            let media_mime = detect_inline_media_mime(map);
            if let Some(mime) = media_mime
                && let Some(Value::String(data)) = map.get_mut("data")
                && data.len() >= BASE64_SNIFF_MIN_BYTES
            {
                *data = base64_placeholder(&mime, data.len());
            }
            for (_, v) in map.iter_mut() {
                sanitize_json_value(v);
            }
        }
        _ => {}
    }
}

fn detect_inline_media_mime(map: &serde_json::Map<String, Value>) -> Option<String> {
    if map.get("type").and_then(Value::as_str) == Some("base64") {
        let mime = map
            .get("media_type")
            .and_then(Value::as_str)
            .unwrap_or("media");
        return Some(mime.to_owned());
    }
    if let Some(mime) = map
        .get("mimeType")
        .or_else(|| map.get("mime_type"))
        .and_then(Value::as_str)
    {
        return Some(mime.to_owned());
    }
    if let Some(fmt) = map.get("format").and_then(Value::as_str)
        && map.contains_key("data")
    {
        return Some(format!("audio/{fmt}"));
    }
    None
}

/// 解析 JSON 并剥离 Base64 后安全截断；非 JSON 原样按 UTF-8 截断。
#[must_use]
pub fn sanitize_and_truncate_json(raw: &[u8], max_bytes: usize) -> (String, bool) {
    if let Ok(mut parsed) = serde_json::from_slice::<Value>(raw) {
        sanitize_json_value(&mut parsed);
        let compact = parsed.to_string();
        return truncate_utf8(&compact, max_bytes);
    }
    let lossy = String::from_utf8_lossy(raw);
    truncate_utf8(&lossy, max_bytes)
}

#[derive(Default)]
struct ToolSlot {
    id: String,
    name: String,
    arguments: String,
}

fn serialize_tool_slots(
    slots: &BTreeMap<usize, ToolSlot>,
    max_bytes: usize,
    truncated: &mut bool,
) -> String {
    if slots.is_empty() {
        return String::new();
    }
    let arr: Vec<Value> = slots
        .values()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "arguments": s.arguments,
            })
        })
        .collect();
    let raw = Value::Array(arr).to_string();
    let (out, was_trunc) = truncate_utf8(&raw, max_bytes);
    *truncated |= was_trunc;
    out
}

/// 非流式响应体提取与清洗（覆盖 Chat / Responses / Messages / Gemini）。
#[must_use]
pub fn extract_non_stream(
    ingress: Ingress,
    raw_body: &[u8],
    max_body_bytes: usize,
) -> CapturedResponse {
    let (response_body, mut truncated) = sanitize_and_truncate_json(raw_body, max_body_bytes);
    let Ok(val) = serde_json::from_slice::<Value>(raw_body) else {
        return CapturedResponse {
            response_body,
            truncated,
            ..Default::default()
        };
    };

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut slots: BTreeMap<usize, ToolSlot> = BTreeMap::new();

    match ingress {
        Ingress::OpenAi => {
            if let Some(msg) = val.pointer("/choices/0/message") {
                extract_chat_message(msg, max_body_bytes, &mut text, &mut reasoning, &mut slots, &mut truncated);
            }
        }
        Ingress::Responses | Ingress::ResponsesCompact => {
            if let Some(items) = val.get("output").and_then(Value::as_array) {
                extract_responses_items(items, max_body_bytes, &mut text, &mut reasoning, &mut slots, &mut truncated);
            }
        }
        Ingress::Anthropic => {
            if let Some(blocks) = val.get("content").and_then(Value::as_array) {
                extract_anthropic_blocks(blocks, max_body_bytes, &mut text, &mut reasoning, &mut slots, &mut truncated);
            }
        }
        Ingress::Gemini => {
            if let Some(parts) = val.pointer("/candidates/0/content/parts").and_then(Value::as_array) {
                extract_gemini_parts(parts, max_body_bytes, &mut text, &mut reasoning, &mut slots, &mut truncated);
            }
        }
    }

    let tool_calls = serialize_tool_slots(&slots, max_body_bytes, &mut truncated);
    CapturedResponse {
        response_body,
        response_text: text,
        reasoning_text: reasoning,
        tool_calls,
        truncated,
    }
}

fn extract_chat_message(
    msg: &Value,
    max_bytes: usize,
    text: &mut String,
    reasoning: &mut String,
    slots: &mut BTreeMap<usize, ToolSlot>,
    truncated: &mut bool,
) {
    if let Some(c) = msg.get("content").and_then(Value::as_str) {
        push_capped(text, c, max_bytes, truncated);
    }
    if let Some(r) = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
        .and_then(Value::as_str)
    {
        push_capped(reasoning, r, max_bytes, truncated);
    }
    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for (idx, call) in calls.iter().enumerate() {
            slots.insert(
                idx,
                ToolSlot {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                },
            );
        }
    }
}

fn extract_anthropic_blocks(
    blocks: &[Value],
    max_bytes: usize,
    text: &mut String,
    reasoning: &mut String,
    slots: &mut BTreeMap<usize, ToolSlot>,
    truncated: &mut bool,
) {
    for (idx, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    push_capped(text, t, max_bytes, truncated);
                }
            }
            Some("thinking") => {
                if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                    push_capped(reasoning, t, max_bytes, truncated);
                }
            }
            Some("tool_use") => {
                slots.insert(
                    idx,
                    ToolSlot {
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        arguments: block
                            .get("input")
                            .map_or_else(String::new, Value::to_string),
                    },
                );
            }
            _ => {}
        }
    }
}

fn extract_responses_items(
    items: &[Value],
    max_bytes: usize,
    text: &mut String,
    reasoning: &mut String,
    slots: &mut BTreeMap<usize, ToolSlot>,
    truncated: &mut bool,
) {
    for (idx, item) in items.iter().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(t) = part.get("text").and_then(Value::as_str) {
                            push_capped(text, t, max_bytes, truncated);
                        }
                    }
                }
            }
            Some("reasoning") => {
                if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                    for s in summary {
                        if let Some(t) = s.get("text").and_then(Value::as_str) {
                            push_capped(reasoning, t, max_bytes, truncated);
                        }
                    }
                }
            }
            Some("function_call") => {
                slots.insert(
                    idx,
                    ToolSlot {
                        id: item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        arguments: item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    },
                );
            }
            _ => {}
        }
    }
}

fn extract_gemini_parts(
    parts: &[Value],
    max_bytes: usize,
    text: &mut String,
    reasoning: &mut String,
    slots: &mut BTreeMap<usize, ToolSlot>,
    truncated: &mut bool,
) {
    for (idx, part) in parts.iter().enumerate() {
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                push_capped(reasoning, t, max_bytes, truncated);
            } else {
                push_capped(text, t, max_bytes, truncated);
            }
        }
        if let Some(fc) = part.get("functionCall") {
            slots.insert(
                idx,
                ToolSlot {
                    id: String::new(),
                    name: fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: fc.get("args").map_or_else(String::new, Value::to_string),
                },
            );
        }
    }
}

/// 流式响应增量拼装器：单流严格受限于 `max_body_bytes`，绝不无限增长内存。
pub struct StreamCollector {
    ingress: Ingress,
    max_bytes: usize,
    text: String,
    reasoning: String,
    slots: BTreeMap<usize, ToolSlot>,
    truncated: bool,
}

impl StreamCollector {
    #[must_use]
    pub fn new(ingress: Ingress, max_bytes: usize) -> Self {
        Self {
            ingress,
            max_bytes,
            text: String::new(),
            reasoning: String::new(),
            slots: BTreeMap::new(),
            truncated: false,
        }
    }

    pub fn push(&mut self, raw: &str) {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        match self.ingress {
            Ingress::OpenAi => self.push_chat(&v),
            Ingress::Responses | Ingress::ResponsesCompact => self.push_responses(&v),
            Ingress::Anthropic => self.push_messages(&v),
            Ingress::Gemini => self.push_gemini(&v),
        }
    }

    fn push_chat(&mut self, v: &Value) {
        let Some(delta) = v.pointer("/choices/0/delta") else {
            return;
        };
        if let Some(c) = delta.get("content").and_then(Value::as_str) {
            push_capped(&mut self.text, c, self.max_bytes, &mut self.truncated);
        }
        if let Some(r) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
        {
            push_capped(&mut self.reasoning, r, self.max_bytes, &mut self.truncated);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (pos, call) in calls.iter().enumerate() {
                let idx = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(pos);
                let slot = self.slots.entry(idx).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str)
                    && !id.is_empty()
                {
                    id.clone_into(&mut slot.id);
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    slot.name.push_str(name);
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    push_capped(&mut slot.arguments, args, self.max_bytes, &mut self.truncated);
                }
            }
        }
    }

    fn push_responses(&mut self, v: &Value) {
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "response.output_text.delta" => {
                if let Some(d) = v.get("delta").and_then(Value::as_str) {
                    push_capped(&mut self.text, d, self.max_bytes, &mut self.truncated);
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(d) = v.get("delta").and_then(Value::as_str) {
                    push_capped(&mut self.reasoning, d, self.max_bytes, &mut self.truncated);
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                if v.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let idx = v
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(0);
                    let slot = self.slots.entry(idx).or_default();
                    if let Some(id) = v
                        .pointer("/item/call_id")
                        .or_else(|| v.pointer("/item/id"))
                        .and_then(Value::as_str)
                    {
                        id.clone_into(&mut slot.id);
                    }
                    if let Some(name) = v.pointer("/item/name").and_then(Value::as_str) {
                        name.clone_into(&mut slot.name);
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let idx = v
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(0);
                if let Some(d) = v.get("delta").and_then(Value::as_str) {
                    let slot = self.slots.entry(idx).or_default();
                    push_capped(&mut slot.arguments, d, self.max_bytes, &mut self.truncated);
                }
            }
            _ => {}
        }
    }

    fn push_messages(&mut self, v: &Value) {
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "content_block_start" => {
                let idx = v
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(0);
                if v.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                    let slot = self.slots.entry(idx).or_default();
                    if let Some(id) = v.pointer("/content_block/id").and_then(Value::as_str) {
                        id.clone_into(&mut slot.id);
                    }
                    if let Some(name) = v.pointer("/content_block/name").and_then(Value::as_str) {
                        name.clone_into(&mut slot.name);
                    }
                }
            }
            "content_block_delta" => {
                let idx = v
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(0);
                match v.pointer("/delta/type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = v.pointer("/delta/text").and_then(Value::as_str) {
                            push_capped(&mut self.text, t, self.max_bytes, &mut self.truncated);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = v.pointer("/delta/thinking").and_then(Value::as_str) {
                            push_capped(
                                &mut self.reasoning,
                                t,
                                self.max_bytes,
                                &mut self.truncated,
                            );
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(p) = v.pointer("/delta/partial_json").and_then(Value::as_str) {
                            let slot = self.slots.entry(idx).or_default();
                            push_capped(
                                &mut slot.arguments,
                                p,
                                self.max_bytes,
                                &mut self.truncated,
                            );
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn push_gemini(&mut self, v: &Value) {
        if let Some(parts) = v
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
        {
            extract_gemini_parts(
                parts,
                self.max_bytes,
                &mut self.text,
                &mut self.reasoning,
                &mut self.slots,
                &mut self.truncated,
            );
        }
    }

    #[must_use]
    pub fn finish(mut self) -> CapturedResponse {
        let tool_calls = serialize_tool_slots(&self.slots, self.max_bytes, &mut self.truncated);
        let summary = json!({
            "stream": true,
            "text": self.text,
            "reasoning": self.reasoning,
            "tool_calls": if tool_calls.is_empty() {
                Value::Array(vec![])
            } else {
                serde_json::from_str(&tool_calls).unwrap_or_else(|_| Value::Array(vec![]))
            }
        })
        .to_string();
        let (response_body, body_trunc) = truncate_utf8(&summary, self.max_bytes);
        CapturedResponse {
            response_body,
            response_text: self.text,
            reasoning_text: self.reasoning,
            tool_calls,
            truncated: self.truncated || body_trunc,
        }
    }
}

/// 组装完整归档记录并在后台异步落盘（受优雅下线 drain 追踪，永不阻塞客户端返回）。
pub struct ArchiveMeta {
    pub request_id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub channel_id: i64,
    pub model: String,
    pub requested_model: String,
    pub endpoint: String,
    pub is_stream: bool,
    pub status_code: i16,
    pub error_code: String,
}

pub fn spawn_persist(
    state: &AppState,
    meta: ArchiveMeta,
    capture: ArchiveCapture,
    response: CapturedResponse,
) {
    let pg = state.pg.clone();
    let ch = state.ch.clone();
    let record = ArchiveRecord {
        request_id: meta.request_id,
        user_id: meta.user_id,
        api_key_id: meta.api_key_id,
        channel_id: meta.channel_id,
        model: meta.model,
        requested_model: meta.requested_model,
        endpoint: meta.endpoint,
        is_stream: meta.is_stream,
        status_code: meta.status_code,
        error_code: meta.error_code,
        request_body: capture.request_body,
        response_body: response.response_body,
        response_text: response.response_text,
        reasoning_text: response.reasoning_text,
        tool_calls: response.tool_calls,
        truncated: capture.request_truncated || response.truncated,
        created_at: Utc::now(),
        storage: String::new(),
    };
    state.settlements.spawn(async move {
        if let Err(err) = okapi_store::conversation_archive::insert(&pg, ch.as_ref(), &record).await
        {
            tracing::warn!(
                request_id = %record.request_id,
                error = %err,
                "对话存档异步落盘失败（不影响请求结算）"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_base64_media_and_truncates_utf8_safely() {
        let big_b64 = "A".repeat(4096);
        let req = json!({
            "model": "gpt-5",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "描述这张图"},
                        {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{big_b64}")}}
                    ]
                }
            ]
        });
        let cap = prepare_request(&serde_json::to_vec(&req).unwrap(), 1024);
        assert!(!cap.request_truncated);
        assert!(cap.request_body.contains("[base64 image/png omitted"));
        assert!(!cap.request_body.contains(&big_b64));

        let (trunc, flag) = truncate_utf8("你好世界abc", 7);
        assert!(flag);
        assert_eq!(trunc, "你好");
    }

    #[test]
    fn stream_collector_assembles_chat_and_anthropic_events() {
        let mut chat = StreamCollector::new(Ingress::OpenAi, 4096);
        chat.push(r#"{"choices":[{"delta":{"reasoning_content":"先算","content":"你好"}}]}"#);
        chat.push(r#"{"choices":[{"delta":{"content":"世界","tool_calls":[{"index":0,"id":"call_1","function":{"name":"calc","arguments":"{\"x\":1}"}}]}}]}"#);
        let out = chat.finish();
        assert_eq!(out.reasoning_text, "先算");
        assert_eq!(out.response_text, "你好世界");
        assert!(out.tool_calls.contains("\"name\":\"calc\""));

        let mut msg = StreamCollector::new(Ingress::Anthropic, 4096);
        msg.push(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"step 1"}}"#);
        msg.push(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"42"}}"#);
        let out2 = msg.finish();
        assert_eq!(out2.reasoning_text, "step 1");
        assert_eq!(out2.response_text, "42");
    }
}
