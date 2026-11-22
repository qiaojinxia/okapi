//! OpenAI Chat Completions 入口 → OpenAI Responses 上游反向转换：
//! 请求 Chat → Responses（`messages` → `instructions` + `input`，`tools` 扁平化，`response_format` → `text.format`）；
//! 响应与 SSE 流 Responses → Chat（`output` / `response.*.delta` → `chat.completion` / `chat.completion.chunk`）。
//!
//! 供只支持 Responses 协议的上游（如设置 `capabilities.chat_completions = false` 的 Responses-only 渠道，
//! 或显式开启 `capabilities.chat_via_responses = true` 的 Codex 渠道）复用；Anthropic Messages / Gemini 入口
//! 通过既有 `anthropic_to_openai` / `gemini_to_openai` 与本模块两跳组合完成转换。

use crate::error::UpstreamError;
use crate::responses::usage_from_responses;
use bytes::Bytes;
use okapi_api::UsageProbe;
use serde_json::{Map, Value, json};

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
        "prompt_cache_retention",
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

mod stream;
pub use stream::{ResponsesStreamToChat, wrap_responses_as_chat};

#[cfg(test)]
mod tests;
