//! 对话内容存档采集器：请求/响应体清洗（剥离图片/音频 Base64 占位）、安全 UTF-8 截断、
//! 多协议流式增量拼装（OpenAI Chat / Responses / Anthropic Messages / Gemini）与异步落盘。

use crate::ingress::Ingress;
use crate::state::AppState;
use chrono::Utc;
use okapi_store::ChClient;
use okapi_store::conversation_archive::ArchiveRecord;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const BASE64_SNIFF_MIN_BYTES: usize = 256;

/// 有界异步批写队列上限：CH/PG 抖动时快速丢弃并告警，永不拖垮网关内存或财务结算。
const ARCHIVE_QUEUE_CAPACITY: usize = 2048;
/// 单批最大条数与近似字节预算：小对话尽量合并减少 ClickHouse MergeTree part，大对话按字节提前落盘。
const ARCHIVE_BATCH_MAX_ITEMS: usize = 64;
const ARCHIVE_BATCH_MAX_BYTES: usize = 2 * 1024 * 1024;
/// 批聚合等待窗：低流量下最多延迟 200ms 落盘，高流量下按条数/字节立即触发。
const ARCHIVE_COALESCE_WINDOW: Duration = Duration::from_millis(200);
/// 队列满丢弃日志节流间隔，避免背压期间刷屏。
const ARCHIVE_DROP_WARN_INTERVAL_SECS: u64 = 10;

/// 下线时等待对话存档微批刷盘的上限：独立于财务结算 `SETTLE_DRAIN_CAP`，
/// 避免慢速存档存储拖住财务结算或超过容器停机宽限期。
pub const ARCHIVE_DRAIN_CAP: Duration = Duration::from_secs(5);

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

/// 复用调用方已解析好的请求 `Value` 生成存档快照：
/// 不含内联 Base64 时零克隆直接限长序列化；含内联 Base64 时仅克隆一次并原地剥离。
#[must_use]
pub fn prepare_request_value(parsed: &Value, max_body_bytes: usize) -> ArchiveCapture {
    let (request_body, request_truncated) = sanitize_and_truncate_value(parsed, max_body_bytes);
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
            let has_large_data = map
                .get("data")
                .and_then(Value::as_str)
                .is_some_and(|d| d.len() >= BASE64_SNIFF_MIN_BYTES);
            if has_large_data
                && let Some(mime) = detect_inline_media_mime(map)
                && let Some(Value::String(data)) = map.get_mut("data")
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

fn has_inline_media_data(map: &serde_json::Map<String, Value>) -> bool {
    let Some(Value::String(data)) = map.get("data") else {
        return false;
    };
    if data.len() < BASE64_SNIFF_MIN_BYTES {
        return false;
    }
    map.get("type").and_then(Value::as_str) == Some("base64")
        || map
            .get("mimeType")
            .or_else(|| map.get("mime_type"))
            .and_then(Value::as_str)
            .is_some()
        || map.get("format").and_then(Value::as_str).is_some()
}

fn needs_sanitization(value: &Value) -> bool {
    match value {
        Value::String(s) => {
            s.len() >= BASE64_SNIFF_MIN_BYTES && s.starts_with("data:") && s.contains(";base64,")
        }
        Value::Array(items) => items.iter().any(needs_sanitization),
        Value::Object(map) => has_inline_media_data(map) || map.values().any(needs_sanitization),
        _ => false,
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

/// 限长 JSON 写入器：写满 `limit` 后立即中止 `serde_json::to_writer`，避免大请求体先完整分配几 MB 字符串再切片。
struct CappedWriter {
    buf: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl CappedWriter {
    fn new(limit: usize) -> Self {
        Self {
            buf: Vec::with_capacity(limit.min(4096)),
            limit,
            truncated: false,
        }
    }

    fn into_utf8_string(mut self) -> (String, bool) {
        if let Err(err) = std::str::from_utf8(&self.buf) {
            self.buf.truncate(err.valid_up_to());
        }
        let text = String::from_utf8(self.buf).unwrap_or_default();
        (text, self.truncated)
    }
}

impl io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        let remaining = self.limit.saturating_sub(self.buf.len());
        if remaining == 0 {
            self.truncated = true;
            return Err(io::Error::other("capped"));
        }
        if data.len() <= remaining {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        } else {
            self.buf.extend_from_slice(&data[..remaining]);
            self.truncated = true;
            Err(io::Error::other("capped"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_value_capped(value: &Value, max_bytes: usize) -> (String, bool) {
    let mut writer = CappedWriter::new(max_bytes);
    let _ = serde_json::to_writer(&mut writer, value);
    writer.into_utf8_string()
}

fn sanitize_and_truncate_value(parsed: &Value, max_bytes: usize) -> (String, bool) {
    if !needs_sanitization(parsed) {
        return serialize_value_capped(parsed, max_bytes);
    }
    let mut cloned = parsed.clone();
    sanitize_json_value(&mut cloned);
    serialize_value_capped(&cloned, max_bytes)
}

/// 解析 JSON 并剥离 Base64 后安全截断；非 JSON 原样按 UTF-8 截断。
#[must_use]
pub fn sanitize_and_truncate_json(raw: &[u8], max_bytes: usize) -> (String, bool) {
    if let Ok(mut parsed) = serde_json::from_slice::<Value>(raw) {
        sanitize_json_value(&mut parsed);
        return serialize_value_capped(&parsed, max_bytes);
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

/// 非流式响应体提取与清洗（覆盖 Chat / Responses / Messages / Gemini），单次 JSON 解析完成提取与脱敏。
#[must_use]
pub fn extract_non_stream(
    ingress: Ingress,
    raw_body: &[u8],
    max_body_bytes: usize,
) -> CapturedResponse {
    let Ok(mut val) = serde_json::from_slice::<Value>(raw_body) else {
        let lossy = String::from_utf8_lossy(raw_body);
        let (response_body, truncated) = truncate_utf8(&lossy, max_body_bytes);
        return CapturedResponse {
            response_body,
            truncated,
            ..Default::default()
        };
    };

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut slots: BTreeMap<usize, ToolSlot> = BTreeMap::new();
    let mut truncated = false;

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
    sanitize_json_value(&mut val);
    let (response_body, body_truncated) = serialize_value_capped(&val, max_body_bytes);
    truncated |= body_truncated;
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

/// 组装完整归档记录并投入有界微批落盘队列（独立于财务结算 `settlements`，永不阻塞客户端返回）。
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

type BatchFlusher =
    Arc<dyn Fn(Vec<ArchiveRecord>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

enum ArchiveCommand {
    Record(Box<ArchiveRecord>),
    Flush(oneshot::Sender<()>),
}

enum BatcherState {
    Idle,
    Running {
        tx: mpsc::Sender<ArchiveCommand>,
        handle: tokio::task::JoinHandle<()>,
    },
    Drained,
}

#[derive(Clone, Copy)]
struct BatcherConfig {
    queue_capacity: usize,
    max_items: usize,
    max_bytes: usize,
    coalesce_window: Duration,
}

impl Default for BatcherConfig {
    fn default() -> Self {
        Self {
            queue_capacity: ARCHIVE_QUEUE_CAPACITY,
            max_items: ARCHIVE_BATCH_MAX_ITEMS,
            max_bytes: ARCHIVE_BATCH_MAX_BYTES,
            coalesce_window: ARCHIVE_COALESCE_WINDOW,
        }
    }
}

/// 对话存档的进程内有界微批落盘器：
/// - 懒启动：存档关闭时零后台协程、零内存占用；
/// - 有界背压：队列满时立即丢弃并节流告警，不拖垮内存也不挤占财务结算 `settlements`；
/// - 攒批落盘：按条数（64）、近似字节上限（2 MiB）或 200ms 窗口批量写入 ClickHouse / PG；
/// - 独立优雅下线：`drain(cap)` 关闭通道并冲刷余批，超时自动中止。
pub struct ArchiveBatcher {
    config: BatcherConfig,
    state: Mutex<BatcherState>,
    dropped: AtomicU64,
    last_drop_warn_secs: AtomicU64,
}

impl Default for ArchiveBatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveBatcher {
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(BatcherConfig::default())
    }

    fn with_config(config: BatcherConfig) -> Self {
        Self {
            config,
            state: Mutex::new(BatcherState::Idle),
            dropped: AtomicU64::new(0),
            last_drop_warn_secs: AtomicU64::new(0),
        }
    }

    /// 将单条存档投递到后台批写队列；返回 `false` 表示队列已满或正在下线（已丢弃并计数）。
    pub fn enqueue(&self, pg: &PgPool, ch: Option<&ChClient>, record: ArchiveRecord) -> bool {
        self.enqueue_with(record, || default_flusher(pg.clone(), ch.cloned()))
    }

    fn enqueue_with(
        &self,
        record: ArchiveRecord,
        make_flusher: impl FnOnce() -> BatchFlusher,
    ) -> bool {
        let tx = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*guard {
                BatcherState::Running { tx, .. } => tx.clone(),
                BatcherState::Drained => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                BatcherState::Idle => {
                    let (tx, rx) = mpsc::channel(self.config.queue_capacity);
                    let flusher = make_flusher();
                    let handle = tokio::spawn(run_archive_batcher(rx, flusher, self.config));
                    *guard = BatcherState::Running {
                        tx: tx.clone(),
                        handle,
                    };
                    tx
                }
            }
        };

        match tx.try_send(ArchiveCommand::Record(Box::new(record))) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                let total_dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                self.warn_dropped_throttled(total_dropped);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// 主动冲刷当前已入队的全部存档并等待落盘（主要供测试与诊断使用；不关闭批写器）。
    pub async fn flush(&self, cap: Duration) -> bool {
        let tx = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*guard {
                BatcherState::Running { tx, .. } => tx.clone(),
                BatcherState::Idle | BatcherState::Drained => return true,
            }
        };
        let (done_tx, done_rx) = oneshot::channel();
        if tx.send(ArchiveCommand::Flush(done_tx)).await.is_err() {
            return false;
        }
        tokio::time::timeout(cap, done_rx).await.is_ok()
    }

    /// 优雅停机冲刷：停止接收新存档、等待队列余批落盘（最多 `cap`），超时中止并告警。
    pub async fn drain(&self, cap: Duration) {
        let running = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match std::mem::replace(&mut *guard, BatcherState::Drained) {
                BatcherState::Running { tx, handle } => Some((tx, handle)),
                BatcherState::Idle | BatcherState::Drained => None,
            }
        };
        let Some((tx, mut handle)) = running else {
            return;
        };
        drop(tx);
        if tokio::time::timeout(cap, &mut handle).await.is_err() {
            tracing::warn!(
                timeout_secs = cap.as_secs(),
                "对话存档批写器未在下线窗口内完成余批落盘，已中止等待"
            );
            handle.abort();
        }
    }

    /// 自进程启动以来因背压或下线丢弃的存档条数。
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn warn_dropped_throttled(&self, total_dropped: u64) {
        let now = monotonic_secs();
        let prev = self.last_drop_warn_secs.load(Ordering::Relaxed);
        if (prev == 0 || now.saturating_sub(prev) >= ARCHIVE_DROP_WARN_INTERVAL_SECS)
            && self
                .last_drop_warn_secs
                .compare_exchange(prev, now.max(1), Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            tracing::warn!(
                dropped = total_dropped,
                queue_capacity = self.config.queue_capacity,
                "对话存档批写队列已满，已丢弃新存档以保护主链路"
            );
        }
    }
}

fn monotonic_secs() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs()
}

fn default_flusher(pg: PgPool, ch: Option<ChClient>) -> BatchFlusher {
    Arc::new(move |batch: Vec<ArchiveRecord>| {
        let pg = pg.clone();
        let ch = ch.clone();
        Box::pin(async move {
            let count = batch.len();
            if let Err(err) =
                okapi_store::conversation_archive::insert_batch(&pg, ch.as_ref(), &batch).await
            {
                tracing::warn!(
                    count,
                    error = %err,
                    "对话存档批量落盘失败（不影响请求结算）"
                );
            }
        })
    })
}

fn record_approx_bytes(r: &ArchiveRecord) -> usize {
    r.request_body
        .len()
        .saturating_add(r.response_body.len())
        .saturating_add(r.response_text.len())
        .saturating_add(r.reasoning_text.len())
        .saturating_add(r.tool_calls.len())
        .saturating_add(256)
}

async fn run_archive_batcher(
    mut rx: mpsc::Receiver<ArchiveCommand>,
    flusher: BatchFlusher,
    config: BatcherConfig,
) {
    let mut pending: Vec<ArchiveRecord> = Vec::with_capacity(config.max_items);
    let mut pending_bytes: usize = 0;
    let mut flush_waiters: Vec<oneshot::Sender<()>> = Vec::new();

    while let Some(first) = rx.recv().await {
        match first {
            ArchiveCommand::Flush(tx) => {
                let _ = tx.send(());
                continue;
            }
            ArchiveCommand::Record(rec) => {
                pending_bytes = pending_bytes.saturating_add(record_approx_bytes(&rec));
                pending.push(*rec);
            }
        }

        let mut flush_now = false;
        while !flush_now && pending.len() < config.max_items && pending_bytes < config.max_bytes {
            match rx.try_recv() {
                Ok(ArchiveCommand::Record(rec)) => {
                    pending_bytes = pending_bytes.saturating_add(record_approx_bytes(&rec));
                    pending.push(*rec);
                }
                Ok(ArchiveCommand::Flush(tx)) => {
                    flush_waiters.push(tx);
                    flush_now = true;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    flush_now = true;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
            }
        }

        if !flush_now && pending.len() < config.max_items && pending_bytes < config.max_bytes {
            let deadline = tokio::time::Instant::now() + config.coalesce_window;
            while !flush_now && pending.len() < config.max_items && pending_bytes < config.max_bytes
            {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(ArchiveCommand::Record(rec))) => {
                        pending_bytes = pending_bytes.saturating_add(record_approx_bytes(&rec));
                        pending.push(*rec);
                    }
                    Ok(Some(ArchiveCommand::Flush(tx))) => {
                        flush_waiters.push(tx);
                        flush_now = true;
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }

        let batch = std::mem::take(&mut pending);
        pending_bytes = 0;
        let batch_len = batch.len();
        if futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(flusher(batch)))
            .await
            .is_err()
        {
            tracing::error!(
                batch_len,
                "conversation archive batcher panicked; continuing with subsequent batches"
            );
        }
        for waiter in flush_waiters.drain(..) {
            let _ = waiter.send(());
        }
    }
}

pub fn spawn_persist(
    state: &AppState,
    meta: ArchiveMeta,
    capture: ArchiveCapture,
    response: CapturedResponse,
) {
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
    state
        .flow
        .archive_batcher
        .enqueue(&state.pg, state.ch.as_ref(), record);
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

    #[test]
    fn prepare_request_value_matches_bytes_path_and_truncates_utf8_boundaries() {
        let plain = json!({
            "model": "gpt-5",
            "messages": [{"role": "user", "content": "你好世界hello"}]
        });
        let raw_plain = serde_json::to_vec(&plain).unwrap();
        for cap_bytes in [0usize, 5, 16, 40, raw_plain.len(), 1024] {
            let from_bytes = prepare_request(&raw_plain, cap_bytes);
            let from_value = prepare_request_value(&plain, cap_bytes);
            assert_eq!(from_value.request_body, from_bytes.request_body);
            assert_eq!(from_value.request_truncated, from_bytes.request_truncated);
            assert!(from_value.request_body.len() <= cap_bytes);
        }

        let big_b64 = "B".repeat(2048);
        let with_media = json!({
            "model": "claude-sonnet",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/jpeg", "data": big_b64}
                }]
            }]
        });
        let raw_media = serde_json::to_vec(&with_media).unwrap();
        let from_bytes = prepare_request(&raw_media, 1024);
        let from_value = prepare_request_value(&with_media, 1024);
        assert_eq!(from_value.request_body, from_bytes.request_body);
        assert!(from_value.request_body.contains("[base64 image/jpeg omitted"));
        assert!(!from_value.request_body.contains(&big_b64));
    }

    #[test]
    fn extract_non_stream_parses_once_and_redacts_response_media() {
        let big_b64 = "C".repeat(1024);
        let payload = json!({
            "choices": [{
                "message": {
                    "content": "完成",
                    "reasoning_content": "先想一步",
                    "tool_calls": [{
                        "id": "call_9",
                        "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}
                    }]
                }
            }],
            "attachment": format!("data:image/png;base64,{big_b64}")
        });
        let out = extract_non_stream(Ingress::OpenAi, &serde_json::to_vec(&payload).unwrap(), 4096);
        assert_eq!(out.response_text, "完成");
        assert_eq!(out.reasoning_text, "先想一步");
        assert!(out.tool_calls.contains("\"name\":\"lookup\""));
        assert!(out.response_body.contains("[base64 image/png omitted"));
        assert!(!out.response_body.contains(&big_b64));
    }

    fn sample_record(body_len: usize) -> ArchiveRecord {
        ArchiveRecord {
            request_id: Uuid::new_v4(),
            user_id: 1,
            api_key_id: 2,
            channel_id: 3,
            model: "gpt-5".into(),
            requested_model: "gpt-5".into(),
            endpoint: "/v1/chat/completions".into(),
            is_stream: false,
            status_code: 200,
            error_code: String::new(),
            request_body: "x".repeat(body_len),
            response_body: String::new(),
            response_text: "ok".into(),
            reasoning_text: String::new(),
            tool_calls: String::new(),
            truncated: false,
            created_at: Utc::now(),
            storage: String::new(),
        }
    }

    #[tokio::test]
    async fn archive_batcher_coalesces_splits_and_drains_cleanly() {
        let sizes: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&sizes);
        let flusher: BatchFlusher = Arc::new(move |batch: Vec<ArchiveRecord>| {
            let sink = Arc::clone(&sink);
            Box::pin(async move {
                sink.lock().unwrap().push(batch.len());
            })
        });
        let batcher = ArchiveBatcher::with_config(BatcherConfig {
            queue_capacity: 32,
            max_items: 4,
            max_bytes: 1024 * 1024,
            coalesce_window: Duration::from_secs(5),
        });

        for _ in 0..10 {
            assert!(batcher.enqueue_with(sample_record(16), || Arc::clone(&flusher)));
        }
        batcher.drain(Duration::from_secs(2)).await;

        let recorded = sizes.lock().unwrap().clone();
        assert_eq!(recorded.iter().sum::<usize>(), 10);
        assert!(recorded.iter().all(|&n| n <= 4));
        assert_eq!(batcher.dropped_count(), 0);
        // Post-drain enqueues are dropped without panicking.
        assert!(!batcher.enqueue_with(sample_record(16), || Arc::clone(&flusher)));
        assert_eq!(batcher.dropped_count(), 1);
    }

    #[tokio::test]
    async fn archive_batcher_respects_backpressure_and_survives_flusher_panic() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(AtomicU64::new(0));
        let delivered = Arc::new(AtomicU64::new(0));

        let gate_ref = Arc::clone(&gate);
        let started_ref = Arc::clone(&started);
        let calls_ref = Arc::clone(&calls);
        let delivered_ref = Arc::clone(&delivered);
        let flusher: BatchFlusher = Arc::new(move |batch: Vec<ArchiveRecord>| {
            let gate_ref = Arc::clone(&gate_ref);
            let started_ref = Arc::clone(&started_ref);
            let calls_ref = Arc::clone(&calls_ref);
            let delivered_ref = Arc::clone(&delivered_ref);
            Box::pin(async move {
                let call_idx = calls_ref.fetch_add(1, Ordering::SeqCst);
                if call_idx == 0 {
                    started_ref.notify_one();
                    gate_ref.notified().await;
                    panic!("synthetic archive sink panic");
                }
                delivered_ref.fetch_add(batch.len() as u64, Ordering::SeqCst);
            })
        });

        let batcher = ArchiveBatcher::with_config(BatcherConfig {
            queue_capacity: 2,
            max_items: 1,
            max_bytes: 1024,
            coalesce_window: Duration::from_millis(20),
        });

        // First record enters flusher and blocks inside it.
        assert!(batcher.enqueue_with(sample_record(8), || Arc::clone(&flusher)));
        started.notified().await;

        // Fill bounded queue (cap = 2); next enqueue must be dropped non-blockingly.
        assert!(batcher.enqueue_with(sample_record(8), || Arc::clone(&flusher)));
        assert!(batcher.enqueue_with(sample_record(8), || Arc::clone(&flusher)));
        assert!(!batcher.enqueue_with(sample_record(8), || Arc::clone(&flusher)));
        assert_eq!(batcher.dropped_count(), 1);

        // Unblock the panicking first batch; worker must recover and flush the remaining 2 records.
        gate.notify_one();
        assert!(batcher.flush(Duration::from_secs(2)).await);
        assert_eq!(delivered.load(Ordering::SeqCst), 2);
        batcher.drain(Duration::from_secs(2)).await;
    }
}
