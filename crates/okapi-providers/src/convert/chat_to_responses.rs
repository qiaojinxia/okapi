//! OpenAI Chat Completions 入口 → OpenAI Responses 上游反向转换：
//! 请求 Chat → Responses（`messages` → `instructions` + `input`，`tools` 扁平化，`response_format` → `text.format`）；
//! 响应与 SSE 流 Responses → Chat（`output` / `response.*.delta` → `chat.completion` / `chat.completion.chunk`）。
//!
//! 供只支持 Responses 协议的上游（如设置 `capabilities.chat_completions = false` 的 Responses-only 渠道，
//! 或显式开启 `capabilities.chat_via_responses = true` 的 Codex 渠道）复用；Anthropic Messages / Gemini 入口
//! 通过既有 `anthropic_to_openai` / `gemini_to_openai` 与本模块两跳组合完成转换。

use crate::error::UpstreamError;
use crate::openai::{ChatResponse, StreamHandle};
use crate::responses::usage_from_responses;
use crate::types::ChatEvent;
use bytes::Bytes;
use okapi_api::UsageProbe;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

// ---- 请求转换 ----

/// Chat Completions 请求字节 → Responses 请求字节。
pub fn request_chat_to_responses(
    body: &Bytes,
    upstream_model: &str,
) -> Result<Bytes, UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let out = request_chat_to_responses_value(&src, upstream_model)?;
    serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 在已解析 `Value` 上完成 Chat Completions → Responses 请求转换（供单次解析管道直接复用）。
pub fn request_chat_to_responses_value(
    src: &Value,
    upstream_model: &str,
) -> Result<Value, UpstreamError> {
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    super::single_choice(src.get("n"), "n")?;

    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    let mut seen_non_system = false;

    if let Some(messages) = src.get("messages").and_then(Value::as_array) {
        for msg in messages {
            convert_message(msg, &mut instructions, &mut input, &mut seen_non_system)?;
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(upstream_model));
    if !instructions.is_empty() {
        out.insert("instructions".into(), json!(instructions.join("\n\n")));
    }
    out.insert("input".into(), Value::Array(input));

    if let Some(max_tokens) = src
        .get("max_completion_tokens")
        .filter(|v| !v.is_null())
        .or_else(|| src.get("max_tokens").filter(|v| !v.is_null()))
    {
        out.insert("max_output_tokens".into(), max_tokens.clone());
    }
    for key in [
        "temperature",
        "top_p",
        "user",
        "metadata",
        "service_tier",
        "truncation",
        "prompt_cache_key",
    ] {
        if let Some(v) = src.get(key).filter(|v| !v.is_null()) {
            out.insert(key.into(), v.clone());
        }
    }

    if let Some(reasoning) = src.get("reasoning").filter(|v| v.is_object()) {
        out.insert("reasoning".into(), reasoning.clone());
    } else if let Some(effort) = src
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.insert("reasoning".into(), json!({"effort": effort}));
    }

    if let Some(format) = src.get("response_format").filter(|v| !v.is_null())
        && let Some(text_format) = response_format_to_text_format(format)?
    {
        out.insert("text".into(), json!({"format": text_format}));
    }

    if src.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert("stream".into(), json!(true));
    }

    convert_tools(src, &mut out)?;
    Ok(Value::Object(out))
}

fn convert_message(
    msg: &Value,
    instructions: &mut Vec<String>,
    input: &mut Vec<Value>,
    seen_non_system: &mut bool,
) -> Result<(), UpstreamError> {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
    match role {
        "system" | "developer" => {
            if !*seen_non_system && let Some(text) = pure_text_content(msg.get("content")) {
                if !text.is_empty() {
                    instructions.push(text);
                }
                return Ok(());
            }
            let parts = convert_user_content(msg.get("content"))?;
            if !parts.is_empty() {
                input.push(json!({
                    "type": "message",
                    "role": "developer",
                    "content": parts,
                }));
            }
            Ok(())
        }
        "user" => {
            *seen_non_system = true;
            let parts = convert_user_content(msg.get("content"))?;
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": parts,
            }));
            Ok(())
        }
        "assistant" => {
            *seen_non_system = true;
            let mut content = convert_assistant_content(msg.get("content"))?;
            if let Some(refusal) = msg
                .get("refusal")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                content.push(json!({"type": "refusal", "refusal": refusal}));
            }
            if !content.is_empty() {
                input.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": content,
                }));
            }
            for call in msg
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let call_id = call.get("id").and_then(Value::as_str).unwrap_or("");
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("{}");
                input.push(json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments,
                }));
            }
            Ok(())
        }
        "tool" => {
            *seen_non_system = true;
            let call_id = msg
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let output = convert_tool_output(msg.get("content"))?;
            input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output,
            }));
            Ok(())
        }
        other => Err(super::unsupported_part(other)),
    }
}

fn pure_text_content(content: Option<&Value>) -> Option<String> {
    match content {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let mut joined = String::new();
            for part in parts {
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    return None;
                }
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !joined.is_empty() && !text.is_empty() {
                        joined.push('\n');
                    }
                    joined.push_str(text);
                }
            }
            Some(joined)
        }
        Some(_) => None,
    }
}

fn convert_user_content(content: Option<&Value>) -> Result<Vec<Value>, UpstreamError> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![json!({"type": "input_text", "text": text})]),
        Some(Value::Array(parts)) => parts.iter().map(chat_part_to_input).collect(),
        Some(_) => Err(super::unsupported_part("content")),
    }
}

fn chat_part_to_input(part: &Value) -> Result<Value, UpstreamError> {
    match part.get("type").and_then(Value::as_str) {
        Some("text" | "input_text") => Ok(json!({
            "type": "input_text",
            "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
        })),
        Some("image_url") => {
            let (url, detail) = match part.get("image_url") {
                Some(Value::String(url)) => (url.as_str(), None),
                Some(Value::Object(obj)) => (
                    obj.get("url")
                        .and_then(Value::as_str)
                        .ok_or_else(|| super::unsupported_part("image_url"))?,
                    obj.get("detail").and_then(Value::as_str),
                ),
                _ => return Err(super::unsupported_part("image_url")),
            };
            let mut item = Map::new();
            item.insert("type".into(), json!("input_image"));
            item.insert("image_url".into(), json!(url));
            if let Some(detail) = detail {
                item.insert("detail".into(), json!(detail));
            }
            Ok(Value::Object(item))
        }
        Some("input_image" | "input_file") => Ok(part.clone()),
        Some("input_audio") => {
            let audio = part
                .get("input_audio")
                .filter(|v| v.is_object())
                .ok_or_else(|| super::unsupported_part("input_audio"))?;
            Ok(json!({
                "type": "input_audio",
                "input_audio": audio,
            }))
        }
        Some("file") => {
            let file = part
                .get("file")
                .and_then(Value::as_object)
                .ok_or_else(|| super::unsupported_part("file"))?;
            let mut item = Map::new();
            item.insert("type".into(), json!("input_file"));
            let mut has_payload = false;
            for key in ["file_id", "file_data", "filename"] {
                if let Some(v) = file.get(key).filter(|v| !v.is_null()) {
                    if key != "filename" {
                        has_payload = true;
                    }
                    item.insert(key.into(), v.clone());
                }
            }
            if !has_payload {
                return Err(super::unsupported_part("file"));
            }
            Ok(Value::Object(item))
        }
        Some(other) => Err(super::unsupported_part(other)),
        None => Err(super::unsupported_part("content_part")),
    }
}

fn convert_assistant_content(content: Option<&Value>) -> Result<Vec<Value>, UpstreamError> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) if text.is_empty() => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![json!({"type": "output_text", "text": text})]),
        Some(Value::Array(parts)) => {
            let mut out = Vec::with_capacity(parts.len());
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text" | "output_text") => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                        if !text.is_empty() {
                            out.push(json!({"type": "output_text", "text": text}));
                        }
                    }
                    Some("refusal") => {
                        let refusal = part.get("refusal").and_then(Value::as_str).unwrap_or("");
                        if !refusal.is_empty() {
                            out.push(json!({"type": "refusal", "refusal": refusal}));
                        }
                    }
                    Some(other) => return Err(super::unsupported_part(other)),
                    None => return Err(super::unsupported_part("content_part")),
                }
            }
            Ok(out)
        }
        Some(_) => Err(super::unsupported_part("content")),
    }
}

fn convert_tool_output(content: Option<&Value>) -> Result<Value, UpstreamError> {
    match content {
        None | Some(Value::Null) => Ok(json!("")),
        Some(Value::String(text)) => Ok(json!(text)),
        Some(Value::Array(parts)) => {
            if let Some(text) = pure_text_content(content) {
                return Ok(json!(text));
            }
            let converted = parts
                .iter()
                .map(chat_part_to_input)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(converted))
        }
        Some(other) => Ok(json!(other.to_string())),
    }
}

fn response_format_to_text_format(format: &Value) -> Result<Option<Value>, UpstreamError> {
    match format.get("type").and_then(Value::as_str) {
        Some("text") => Ok(None),
        Some("json_object") => Ok(Some(json!({"type": "json_object"}))),
        Some("json_schema") => {
            let inner = format
                .get("json_schema")
                .and_then(Value::as_object)
                .ok_or_else(|| super::unsupported_part("response_format.json_schema"))?;
            let schema = inner
                .get("schema")
                .filter(|s| s.is_object())
                .ok_or_else(|| super::unsupported_part("response_format.json_schema.schema"))?;
            let mut out = Map::new();
            out.insert("type".into(), json!("json_schema"));
            out.insert(
                "name".into(),
                inner
                    .get("name")
                    .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                    .cloned()
                    .unwrap_or_else(|| json!("response")),
            );
            out.insert("schema".into(), schema.clone());
            for key in ["strict", "description"] {
                if let Some(v) = inner.get(key).filter(|v| !v.is_null()) {
                    out.insert(key.into(), v.clone());
                }
            }
            Ok(Some(Value::Object(out)))
        }
        _ => Err(super::unsupported_part("response_format")),
    }
}

fn convert_tools(
    src: &Map<String, Value>,
    out: &mut Map<String, Value>,
) -> Result<(), UpstreamError> {
    let tools: Vec<Value> = src
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return None;
            }
            let func = tool.get("function")?.as_object()?;
            let name = func.get("name")?.as_str()?;
            let mut flat = Map::new();
            flat.insert("type".into(), json!("function"));
            flat.insert("name".into(), json!(name));
            if let Some(desc) = func.get("description").filter(|v| !v.is_null()) {
                flat.insert("description".into(), desc.clone());
            }
            flat.insert(
                "parameters".into(),
                func.get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            );
            if let Some(strict) = func.get("strict").filter(|v| v.is_boolean()) {
                flat.insert("strict".into(), strict.clone());
            }
            Some(Value::Object(flat))
        })
        .collect();
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    if let Some(choice) = src.get("tool_choice").filter(|v| !v.is_null()) {
        out.insert("tool_choice".into(), tool_choice_to_responses(choice)?);
    }
    if out.contains_key("tools")
        && let Some(parallel) = src.get("parallel_tool_calls").filter(|v| v.is_boolean())
    {
        out.insert("parallel_tool_calls".into(), parallel.clone());
    }
    Ok(())
}

fn tool_choice_to_responses(choice: &Value) -> Result<Value, UpstreamError> {
    match choice {
        Value::String(s) if matches!(s.as_str(), "auto" | "none" | "required") => {
            Ok(Value::String(s.clone()))
        }
        Value::Object(obj) if obj.get("type").and_then(Value::as_str) == Some("function") => {
            let name = obj
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| obj.get("name"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| super::unsupported_part("tool_choice"))?;
            Ok(json!({"type": "function", "name": name}))
        }
        _ => Err(super::unsupported_part("tool_choice")),
    }
}

// ---- 响应转换（非流式） ----

/// Responses JSON 响应 → Chat Completions JSON 响应。
pub fn response_responses_to_chat(
    body: &Bytes,
    fallback_model: &str,
) -> Result<(Bytes, Option<UsageProbe>), UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;

    let mut text = String::new();
    let mut refusal = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for item in src
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => append_message_content(item, &mut text, &mut refusal),
            Some("reasoning") => append_reasoning_from_item(item, &mut reasoning),
            Some("function_call") => tool_calls.push(function_call_to_chat(item)),
            _ => {}
        }
    }

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    if text.is_empty() && (!tool_calls.is_empty() || !refusal.is_empty()) {
        message.insert("content".into(), Value::Null);
    } else {
        message.insert("content".into(), json!(text));
    }
    if !refusal.is_empty() {
        message.insert("refusal".into(), json!(refusal));
    }
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    let has_tool_calls = message.contains_key("tool_calls");
    let finish_reason = finish_reason_from_response(&src, has_tool_calls);
    let probe = usage_from_responses(src.get("usage"));
    let usage_json = chat_usage_json(src.get("usage"), probe);

    let id = src
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("chatcmpl-resp");
    let created = src
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let model = src
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_model);

    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("object".into(), json!("chat.completion"));
    out.insert("created".into(), json!(created));
    out.insert("model".into(), json!(model));
    out.insert(
        "choices".into(),
        json!([{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason,
        }]),
    );
    if let Some(usage) = usage_json {
        out.insert("usage".into(), usage);
    }

    let bytes = serde_json::to_vec(&Value::Object(out))
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, probe))
}

fn append_message_content(item: &Value, text: &mut String, refusal: &mut String) {
    for part in item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match part.get("type").and_then(Value::as_str) {
            Some("output_text" | "text") => {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            Some("refusal") => {
                if let Some(r) = part.get("refusal").and_then(Value::as_str) {
                    refusal.push_str(r);
                }
            }
            _ => {}
        }
    }
}

fn function_call_to_chat(item: &Value) -> Value {
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("{}");
    json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        }
    })
}

fn append_reasoning_from_item(item: &Value, dst: &mut String) {
    let mut found_summary = false;
    for part in item
        .get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(t) = part.get("text").and_then(Value::as_str)
            && !t.is_empty()
        {
            if !dst.is_empty() {
                dst.push('\n');
            }
            dst.push_str(t);
            found_summary = true;
        }
    }
    if found_summary {
        return;
    }
    for part in item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(t) = part.get("text").and_then(Value::as_str)
            && !t.is_empty()
        {
            if !dst.is_empty() {
                dst.push('\n');
            }
            dst.push_str(t);
        }
    }
}

fn finish_reason_from_response(resp: &Value, has_tool_calls: bool) -> &'static str {
    if resp.get("status").and_then(Value::as_str) == Some("incomplete") {
        return match resp
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("content_filter") => "content_filter",
            _ => "length",
        };
    }
    if has_tool_calls { "tool_calls" } else { "stop" }
}

fn chat_usage_json(raw: Option<&Value>, probe: Option<UsageProbe>) -> Option<Value> {
    if let Some(v) = probe.map(UsageProbe::chat_json).filter(|v| !v.is_null()) {
        return Some(v);
    }
    let obj = raw?.as_object()?;
    let mut out = Map::new();
    for (src, dst) in [
        ("input_tokens", "prompt_tokens"),
        ("output_tokens", "completion_tokens"),
        ("total_tokens", "total_tokens"),
        ("input_tokens_details", "prompt_tokens_details"),
        ("output_tokens_details", "completion_tokens_details"),
    ] {
        if let Some(v) = obj.get(src).filter(|v| !v.is_null()) {
            out.insert(dst.into(), v.clone());
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

// ---- 流式响应转换 ----

/// Responses SSE 事件流 → OpenAI Chat Completions chunk 流。
pub struct ResponsesStreamToChat {
    id: String,
    created: i64,
    model: String,
    sent_role: bool,
    has_tool_calls: bool,
    finished: bool,
    tool_slots: HashMap<String, usize>,
    next_tool_slot: usize,
    streamed_tool_args: HashSet<usize>,
    streamed_text_items: HashSet<String>,
    streamed_refusal_items: HashSet<String>,
    streamed_reasoning_items: HashSet<String>,
}

impl ResponsesStreamToChat {
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            id: "chatcmpl-resp".to_owned(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_owned(),
            sent_role: false,
            has_tool_calls: false,
            finished: false,
            tool_slots: HashMap::new(),
            next_tool_slot: 0,
            streamed_tool_args: HashSet::new(),
            streamed_text_items: HashSet::new(),
            streamed_refusal_items: HashSet::new(),
            streamed_reasoning_items: HashSet::new(),
        }
    }

    pub fn step(
        &mut self,
        item: Result<ChatEvent, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        match item {
            Err(err) => vec![Err(err)],
            Ok(ChatEvent::Done) => {
                if self.finished {
                    return Vec::new();
                }
                self.finished = true;
                let finish = if self.has_tool_calls {
                    "tool_calls"
                } else {
                    "stop"
                };
                vec![Ok(self.finish_chunk(finish)), Ok(ChatEvent::Done)]
            }
            Ok(ChatEvent::Data { raw, event, .. }) => self.handle_event(&raw, event.as_deref()),
        }
    }

    fn handle_event(
        &mut self,
        raw: &str,
        event_name: Option<&str>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        if raw.trim().is_empty() {
            return Vec::new();
        }
        let Ok(ev) = serde_json::from_str::<Value>(raw) else {
            return vec![Err(UpstreamError::Stream("responses_event_json".into()))];
        };
        let kind = ev
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or(event_name)
            .unwrap_or("");

        match kind {
            "response.created" | "response.in_progress" => {
                if let Some(resp) = ev.get("response") {
                    self.capture_meta(resp);
                }
                Vec::new()
            }
            "response.output_text.delta" => {
                let Some(delta) = ev
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Vec::new();
                };
                if let Some(id) = ev.get("item_id").and_then(Value::as_str) {
                    self.streamed_text_items.insert(id.to_owned());
                }
                vec![Ok(self.delta_chunk(
                    json!({"content": delta}),
                    delta.chars().count(),
                ))]
            }
            "response.refusal.delta" => {
                let Some(delta) = ev
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Vec::new();
                };
                if let Some(id) = ev.get("item_id").and_then(Value::as_str) {
                    self.streamed_refusal_items.insert(id.to_owned());
                }
                vec![Ok(self.delta_chunk(
                    json!({"refusal": delta}),
                    delta.chars().count(),
                ))]
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let Some(delta) = ev
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Vec::new();
                };
                if let Some(id) = ev.get("item_id").and_then(Value::as_str) {
                    self.streamed_reasoning_items.insert(id.to_owned());
                }
                vec![Ok(self.delta_chunk(
                    json!({"reasoning_content": delta}),
                    delta.chars().count(),
                ))]
            }
            "response.output_item.added" => self.handle_item_added(&ev),
            "response.function_call_arguments.delta" => self.handle_tool_args_delta(&ev),
            "response.output_item.done" => self.handle_item_done(&ev),
            "response.completed" | "response.incomplete" => self.handle_terminal(&ev),
            "response.failed" => {
                let msg = ev
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("responses_failed");
                self.finished = true;
                vec![Err(UpstreamError::Stream(msg.to_owned()))]
            }
            _ => Vec::new(),
        }
    }

    fn capture_meta(&mut self, resp: &Value) {
        if let Some(id) = resp
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            id.clone_into(&mut self.id);
        }
        if let Some(created) = resp.get("created_at").and_then(Value::as_i64) {
            self.created = created;
        }
        if let Some(model) = resp
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            model.clone_into(&mut self.model);
        }
    }

    fn slot_key(ev: &Value, item: Option<&Value>) -> String {
        if let Some(id) = item
            .and_then(|i| i.get("id"))
            .or_else(|| ev.get("item_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return id.to_owned();
        }
        ev.get("output_index")
            .and_then(Value::as_u64)
            .map_or_else(|| "0".to_owned(), |n| format!("idx:{n}"))
    }

    fn alloc_tool_slot(&mut self, key: String) -> usize {
        if let Some(&slot) = self.tool_slots.get(&key) {
            return slot;
        }
        let slot = self.next_tool_slot;
        self.next_tool_slot += 1;
        self.tool_slots.insert(key, slot);
        slot
    }

    fn handle_item_added(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(item) = ev.get("item") else {
            return Vec::new();
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Vec::new();
        }
        let key = Self::slot_key(ev, Some(item));
        let slot = self.alloc_tool_slot(key);
        self.has_tool_calls = true;
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let args = item.get("arguments").and_then(Value::as_str).unwrap_or("");
        if !args.is_empty() {
            self.streamed_tool_args.insert(slot);
        }
        vec![Ok(self.delta_chunk(
            json!({
                "tool_calls": [{
                    "index": slot,
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args,
                    }
                }]
            }),
            args.chars().count(),
        ))]
    }

    fn handle_tool_args_delta(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(delta) = ev
            .get("delta")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Vec::new();
        };
        let key = Self::slot_key(ev, None);
        let slot = self.alloc_tool_slot(key);
        self.has_tool_calls = true;
        self.streamed_tool_args.insert(slot);
        vec![Ok(self.delta_chunk(
            json!({
                "tool_calls": [{
                    "index": slot,
                    "function": {
                        "arguments": delta,
                    }
                }]
            }),
            delta.chars().count(),
        ))]
    }

    fn handle_item_done(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(item) = ev.get("item") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let key = Self::slot_key(ev, Some(item));
                let existed = self.tool_slots.contains_key(&key);
                let slot = self.alloc_tool_slot(key);
                self.has_tool_calls = true;
                let args = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                if !existed {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    let args_out = if args.is_empty() { "{}" } else { args };
                    self.streamed_tool_args.insert(slot);
                    out.push(Ok(self.delta_chunk(
                        json!({
                            "tool_calls": [{
                                "index": slot,
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": args_out,
                                }
                            }]
                        }),
                        args_out.chars().count(),
                    )));
                } else if !self.streamed_tool_args.contains(&slot) && !args.is_empty() {
                    self.streamed_tool_args.insert(slot);
                    out.push(Ok(self.delta_chunk(
                        json!({
                            "tool_calls": [{
                                "index": slot,
                                "function": {
                                    "arguments": args,
                                }
                            }]
                        }),
                        args.chars().count(),
                    )));
                }
            }
            Some("message") => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                let mut text = String::new();
                let mut refusal = String::new();
                append_message_content(item, &mut text, &mut refusal);
                if !text.is_empty() && !self.streamed_text_items.contains(id) {
                    out.push(Ok(
                        self.delta_chunk(json!({"content": text}), text.chars().count())
                    ));
                }
                if !refusal.is_empty() && !self.streamed_refusal_items.contains(id) {
                    out.push(Ok(self.delta_chunk(
                        json!({"refusal": refusal}),
                        refusal.chars().count(),
                    )));
                }
            }
            Some("reasoning") => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                if !self.streamed_reasoning_items.contains(id) {
                    let mut summary = String::new();
                    append_reasoning_from_item(item, &mut summary);
                    if !summary.is_empty() {
                        out.push(Ok(self.delta_chunk(
                            json!({"reasoning_content": summary}),
                            summary.chars().count(),
                        )));
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn handle_terminal(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let resp = ev.get("response").unwrap_or(&Value::Null);
        self.capture_meta(resp);
        let finish = finish_reason_from_response(resp, self.has_tool_calls);
        let probe = usage_from_responses(resp.get("usage"));
        let usage_json = chat_usage_json(resp.get("usage"), probe);

        let mut out = vec![Ok(self.finish_chunk(finish))];
        if probe.is_some() || usage_json.is_some() {
            let chunk = json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [],
                "usage": usage_json.unwrap_or(Value::Null),
            });
            out.push(Ok(ChatEvent::Data {
                raw: chunk.to_string(),
                event: None,
                has_output: false,
                content_chars: 0,
                usage: probe,
            }));
        }
        out.push(Ok(ChatEvent::Done));
        out
    }

    fn delta_chunk(&mut self, mut delta: Value, content_chars: usize) -> ChatEvent {
        if !self.sent_role {
            self.sent_role = true;
            if let Some(obj) = delta.as_object_mut() {
                obj.insert("role".into(), json!("assistant"));
            }
        }
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": Value::Null,
            }],
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output: true,
            content_chars,
            usage: None,
        }
    }

    fn finish_chunk(&self, finish_reason: &str) -> ChatEvent {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish_reason,
            }],
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage: None,
        }
    }
}

/// 将 Responses 上游响应（JSON 或 SSE 流）包装回 OpenAI Chat Completions 形状。
pub fn wrap_responses_as_chat(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => {
            let (body, parsed_usage) = response_responses_to_chat(&body, upstream_model)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage: usage.or(parsed_usage),
            })
        }
        ChatResponse::Stream(h) => {
            let mut st = ResponsesStreamToChat::new(upstream_model);
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(st.step(item)));
            Ok(ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_maps_system_tools_multimodal_reasoning_and_json_schema() {
        let req = json!({
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": "Be concise."},
                {"role": "user", "content": [
                    {"type": "text", "text": "What is in this image?"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/a.png", "detail": "high"}}
                ]},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"q\":\"cat\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "orange tabby"}
            ],
            "max_completion_tokens": 512,
            "temperature": 0.2,
            "reasoning_effort": "high",
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "answer",
                    "strict": true,
                    "schema": {"type": "object", "properties": {"label": {"type": "string"}}}
                }
            },
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Search catalog",
                    "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
                }
            }],
            "tool_choice": {"type": "function", "function": {"name": "lookup"}},
            "parallel_tool_calls": false,
            "stream": true,
            "stream_options": {"include_usage": true}
        });

        let out = request_chat_to_responses_value(&req, "gpt-5-upstream").unwrap();
        assert_eq!(out["model"], "gpt-5-upstream");
        assert_eq!(out["instructions"], "Be concise.");
        assert_eq!(out["max_output_tokens"], 512);
        assert_eq!(out["temperature"], 0.2);
        assert_eq!(out["reasoning"]["effort"], "high");
        assert_eq!(out["stream"], true);
        assert!(out.get("stream_options").is_none());
        assert_eq!(out["text"]["format"]["type"], "json_schema");
        assert_eq!(out["text"]["format"]["name"], "answer");
        assert_eq!(out["text"]["format"]["strict"], true);
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tools"][0]["name"], "lookup");
        assert_eq!(
            out["tool_choice"],
            json!({"type": "function", "name": "lookup"})
        );
        assert_eq!(out["parallel_tool_calls"], false);

        let input = out["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(
            input[0]["content"][1]["image_url"],
            "https://example.com/a.png"
        );
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["output"], "orange tabby");
    }

    #[test]
    fn rejects_multi_choice_and_unknown_content_parts() {
        let multi = json!({"model": "x", "n": 2, "messages": [{"role": "user", "content": "hi"}]});
        assert!(matches!(
            request_chat_to_responses_value(&multi, "x"),
            Err(UpstreamError::Build(msg)) if msg == "unsupported_content:n"
        ));

        let bad_part = json!({
            "model": "x",
            "messages": [{"role": "user", "content": [{"type": "video_frame", "url": "x"}]}]
        });
        assert!(matches!(
            request_chat_to_responses_value(&bad_part, "x"),
            Err(UpstreamError::Build(msg)) if msg == "unsupported_content:video_frame"
        ));
    }

    #[test]
    fn non_stream_response_preserves_text_reasoning_tool_calls_and_usage() {
        let body = Bytes::from(
            json!({
                "id": "resp_123",
                "object": "response",
                "created_at": 1_700_000_000,
                "model": "gpt-5",
                "status": "completed",
                "output": [
                    {
                        "type": "reasoning",
                        "id": "rs_1",
                        "summary": [{"type": "summary_text", "text": "thinking step"}]
                    },
                    {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Checking weather."}]
                    },
                    {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_weather",
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Tokyo\"}"
                    }
                ],
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 40,
                    "total_tokens": 140,
                    "input_tokens_details": {"cached_tokens": 60},
                    "output_tokens_details": {"reasoning_tokens": 15}
                }
            })
            .to_string(),
        );
        let (chat_bytes, probe) = response_responses_to_chat(&body, "fallback").unwrap();
        let chat: Value = serde_json::from_slice(&chat_bytes).unwrap();
        assert_eq!(chat["id"], "resp_123");
        assert_eq!(chat["object"], "chat.completion");
        assert_eq!(chat["model"], "gpt-5");
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            chat["choices"][0]["message"]["content"],
            "Checking weather."
        );
        assert_eq!(
            chat["choices"][0]["message"]["reasoning_content"],
            "thinking step"
        );
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_weather"
        );
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(chat["usage"]["prompt_tokens"], 100);
        assert_eq!(chat["usage"]["completion_tokens"], 40);
        assert_eq!(chat["usage"]["prompt_tokens_details"]["cached_tokens"], 60);
        assert_eq!(
            chat["usage"]["completion_tokens_details"]["reasoning_tokens"],
            15
        );
        let u = probe.unwrap().to_token_usage().unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 40);
        assert_eq!(u.cached_tokens, 60);
        assert_eq!(u.reasoning_tokens, 15);
    }

    #[test]
    fn stream_conversion_emits_text_reasoning_tool_calls_usage_and_single_done() {
        let raw_events = [
            (
                "response.created",
                json!({"type": "response.created", "response": {"id": "resp_s1", "created_at": 1_700_000_001, "model": "gpt-5"}}),
            ),
            (
                "response.reasoning_summary_text.delta",
                json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "delta": "plan"}),
            ),
            (
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "item_id": "msg_1", "delta": "Hello "}),
            ),
            (
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "item_id": "msg_1", "delta": "world"}),
            ),
            (
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "ping", "arguments": ""}}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 2, "delta": "{\"a\":1}"}),
            ),
            (
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_s1",
                        "created_at": 1_700_000_001,
                        "model": "gpt-5",
                        "status": "completed",
                        "usage": {"input_tokens": 12, "output_tokens": 8, "total_tokens": 20}
                    }
                }),
            ),
        ];

        let mut conv = ResponsesStreamToChat::new("fallback");
        let mut emitted = Vec::new();
        for (name, payload) in raw_events {
            for ev in crate::responses::parse_event(name, &payload.to_string()) {
                emitted.extend(conv.step(Ok(ev)));
            }
        }

        let done_count = emitted
            .iter()
            .filter(|e| matches!(e, Ok(ChatEvent::Done)))
            .count();
        assert_eq!(
            done_count, 1,
            "completed + trailing parse_event Done should emit exactly one Done"
        );

        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_args = String::new();
        let mut finish_reason = None;
        let mut seen_usage = None;
        for item in emitted {
            if let Ok(ChatEvent::Data { raw, usage, .. }) = item {
                if usage.is_some() {
                    seen_usage = usage;
                }
                let v: Value = serde_json::from_str(&raw).unwrap();
                if let Some(d) = v
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
                {
                    text.push_str(d);
                }
                if let Some(r) = v
                    .pointer("/choices/0/delta/reasoning_content")
                    .and_then(Value::as_str)
                {
                    reasoning.push_str(r);
                }
                if let Some(a) = v
                    .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                    .and_then(Value::as_str)
                {
                    tool_args.push_str(a);
                }
                if let Some(f) = v
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str)
                {
                    finish_reason = Some(f.to_owned());
                }
            }
        }
        assert_eq!(reasoning, "plan");
        assert_eq!(text, "Hello world");
        assert_eq!(tool_args, "{\"a\":1}");
        assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
        let u = seen_usage.unwrap().to_token_usage().unwrap();
        assert_eq!((u.prompt_tokens, u.completion_tokens), (12, 8));
    }

    #[test]
    fn anthropic_messages_composes_cleanly_to_responses_and_back() {
        let anthropic_req = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 256,
            "system": "You are helpful.",
            "messages": [{"role": "user", "content": "Say hi"}],
            "stream": true
        });
        let chat_req = crate::convert::anthropic_to_openai::request_anthropic_to_openai_value(
            &anthropic_req,
            "gpt-5-codex",
        )
        .unwrap();
        let resp_req = request_chat_to_responses_value(&chat_req, "gpt-5-codex").unwrap();
        assert_eq!(resp_req["model"], "gpt-5-codex");
        assert_eq!(resp_req["instructions"], "You are helpful.");
        assert_eq!(resp_req["max_output_tokens"], 256);
        assert_eq!(resp_req["stream"], true);
        assert_eq!(resp_req["input"][0]["content"][0]["text"], "Say hi");

        // Stream back: Responses -> Chat -> Anthropic Messages SSE
        let mut r2c = ResponsesStreamToChat::new("gpt-5-codex");
        let mut c2a = crate::convert::anthropic_to_openai::OaiStreamToAnthropic::new("gpt-5-codex");
        let mut anthropic_events = Vec::new();
        for (name, payload) in [
            (
                "response.created",
                json!({"type": "response.created", "response": {"id": "resp_a1", "created_at": 1_700_000_002, "model": "gpt-5-codex"}}),
            ),
            (
                "response.output_text.delta",
                json!({"type": "response.output_text.delta", "item_id": "m1", "delta": "Hi there!"}),
            ),
            (
                "response.completed",
                json!({"type": "response.completed", "response": {"id": "resp_a1", "status": "completed", "usage": {"input_tokens": 5, "output_tokens": 3}}}),
            ),
        ] {
            for ev in crate::responses::parse_event(name, &payload.to_string()) {
                for chat_ev in r2c.step(Ok(ev)) {
                    anthropic_events.extend(c2a.step(chat_ev));
                }
            }
        }
        let saw_text = anthropic_events.iter().any(|item| {
            matches!(item, Ok(ChatEvent::Data { raw, event: Some(ev), .. }) if ev == "content_block_delta" && raw.contains("Hi there!"))
        });
        let saw_stop = anthropic_events
            .iter()
            .any(|item| matches!(item, Ok(ChatEvent::Data { event: Some(ev), .. }) if ev == "message_stop"));
        assert!(saw_text, "expected Anthropic content_block_delta");
        assert!(saw_stop, "expected Anthropic message_stop");
    }
}
