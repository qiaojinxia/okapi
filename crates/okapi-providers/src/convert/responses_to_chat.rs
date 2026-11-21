//! OpenAI Responses API 入口 → ChatCompletions 降级（IMPLEMENTATION §4.4，#5209）：
//! 请求 Responses→Chat；响应与 chunk 流 Chat→Responses 事件
//! （response.created → output_item.added → content_part.added →
//! output_text.delta* → *.done → response.completed）。
//! usage 口径：input_tokens = prompt（含缓存）、output_tokens = completion。

use crate::error::UpstreamError;
use bytes::Bytes;
use okapi_api::UsageProbe;
use serde_json::{Value, json};

// ---- 请求转换 ----

/// Responses 请求 → Chat 请求。`instructions` → system；`input`（string | items）→ messages。
pub fn request_responses_to_chat(
    body: &Bytes,
    upstream_model: &str,
) -> Result<Bytes, UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let out = request_responses_to_chat_value(&src, upstream_model)?;
    serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 在已解析 `Value` 上完成 Responses → Chat 请求转换（供单次解析管道直接复用）。
pub fn request_responses_to_chat_value(
    src: &Value,
    upstream_model: &str,
) -> Result<Value, UpstreamError> {
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    // 历史引用指向上游 Responses 存储，chat 没有对应物：降级后上下文会整段消失而客户端毫无察觉。
    // 网关只为原生响应建绑定、续聊锁原渠道，能走到这里的是绑定后渠道被改成降级的情形——明确拒绝。
    for key in ["previous_response_id", "conversation"] {
        if src.get(key).is_some_and(|value| !value.is_null()) {
            return Err(super::unsupported_part(key));
        }
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = src.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        messages.push(json!({"role": "system", "content": instructions}));
    }
    match src.get("input") {
        Some(Value::String(text)) => {
            messages.push(json!({"role": "user", "content": text}));
        }
        Some(Value::Array(items)) => {
            for item in items {
                convert_input_item(item, &mut messages)?;
            }
        }
        _ => {}
    }

    let mut out = serde_json::Map::new();
    out.insert("model".into(), json!(upstream_model));
    out.insert("messages".into(), Value::Array(messages));
    if let Some(v) = src.get("max_output_tokens") {
        out.insert("max_tokens".into(), v.clone());
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = src.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    // 结构化输出：text.format 对应 chat 的 response_format。不映射的话客户端要的 JSON 在降级后
    // 悄悄变成自由文本；表达不了的类型照本文件的口径报错。
    if let Some(format) = src
        .get("text")
        .and_then(|text| text.get("format"))
        .filter(|format| !format.is_null())
        && let Some(response_format) = text_format_to_response_format(format)?
    {
        out.insert("response_format".into(), response_format);
    }
    let stream = src.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if stream {
        out.insert("stream".into(), json!(true));
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }
    convert_tools(src, &mut out)?;

    Ok(Value::Object(out))
}

/// Responses `text.format` → chat `response_format`。`text` 是缺省，不必声明；`json_schema` 把
/// 扁平的 name / schema / strict / description 装进 chat 的 `json_schema` 对象（name 在两边都必填，
/// 缺了按 `response` 补，与 Gemini 入口同一口径）。
fn text_format_to_response_format(format: &Value) -> Result<Option<Value>, UpstreamError> {
    match format.get("type").and_then(Value::as_str) {
        Some("text") => Ok(None),
        Some("json_object") => Ok(Some(json!({"type": "json_object"}))),
        Some("json_schema") => {
            let schema = format
                .get("schema")
                .filter(|schema| schema.is_object())
                .ok_or_else(|| super::unsupported_part("text.format.schema"))?;
            let mut inner = serde_json::Map::new();
            inner.insert(
                "name".into(),
                format
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| json!("response")),
            );
            inner.insert("schema".into(), schema.clone());
            for key in ["strict", "description"] {
                if let Some(value) = format.get(key).filter(|value| !value.is_null()) {
                    inner.insert(key.into(), value.clone());
                }
            }
            Ok(Some(
                json!({"type": "json_schema", "json_schema": Value::Object(inner)}),
            ))
        }
        _ => Err(super::unsupported_part("text.format")),
    }
}

/// input item → chat 消息：message 项（文本 / 图片 / 内联文件 / 音频 part）、
/// function_call / function_call_output 项。
fn convert_input_item(item: &Value, messages: &mut Vec<Value>) -> Result<(), UpstreamError> {
    match item.get("type").and_then(Value::as_str) {
        // 缺省视为 message 项（Responses 允许省略 type）
        None | Some("message") => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            match item.get("content") {
                Some(Value::String(text)) => {
                    messages.push(json!({"role": role, "content": text}));
                }
                Some(Value::Array(parts)) => {
                    let converted = parts
                        .iter()
                        .map(input_part_to_chat)
                        .collect::<Result<Vec<_>, _>>()?;
                    if !converted.is_empty() {
                        // 单段文本降级 string content
                        let only_text = converted.len() == 1
                            && converted[0].get("type").and_then(Value::as_str) == Some("text");
                        let content = if only_text {
                            converted[0].get("text").cloned().unwrap_or(Value::Null)
                        } else {
                            Value::Array(converted)
                        };
                        messages.push(json!({"role": role, "content": content}));
                    }
                }
                _ => {}
            }
        }
        Some("function_call") => {
            messages.push(json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                    "type": "function",
                    "function": {
                        "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                        "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                    }
                }]
            }));
        }
        Some("function_call_output") => {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                "content": tool_output_text(item.get("output"))?,
            }));
        }
        // reasoning 是上游自己的推理（多为加密），Chat 没有对应字段；客户端回放整段输出时会带上，
        // 丢掉不改变对话内容
        Some("reasoning") => {}
        // 其余（item_reference 引用的存储项、内置工具调用及其输出等）Chat 表达不了：
        // 悄悄丢掉会让上游看到缺了一截的对话，明确 400
        Some(other) => return Err(super::unsupported_part(&format!("input_item.{other}"))),
    }
    Ok(())
}

/// Responses message part → chat content part；chat 表达不了的（file_id / file_url 引用、
/// 未知类型）报错，见 [`super::unsupported_part`]。
fn input_part_to_chat(part: &Value) -> Result<Value, UpstreamError> {
    let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
    let converted = match kind {
        "input_text" | "output_text" | "text" => Some(json!({
            "type": "text",
            "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
        })),
        "refusal" => Some(json!({
            "type": "text",
            "text": part.get("refusal").and_then(Value::as_str).unwrap_or(""),
        })),
        "input_image" => part
            .get("image_url")
            .and_then(Value::as_str)
            .map(|url| json!({"type": "image_url", "image_url": {"url": url}})),
        "input_file" => part.get("file_data").and_then(Value::as_str).map(|data| {
            let mut file = json!({"file_data": data});
            if let Some(name) = part.get("filename").filter(|name| name.is_string()) {
                file["filename"] = name.clone();
            }
            json!({"type": "file", "file": file})
        }),
        "input_audio" => part
            .get("input_audio")
            .filter(|audio| audio.is_object())
            .map(|audio| json!({"type": "input_audio", "input_audio": audio})),
        _ => None,
    };
    converted.ok_or_else(|| super::unsupported_part(kind))
}

/// `function_call_output.output`：字符串，或 input part 数组。chat 的 tool 消息只收文本，
/// 文本 part 依序拼接；图片 / 文件 part 表达不了，报错而不是丢掉。
fn tool_output_text(output: Option<&Value>) -> Result<String, UpstreamError> {
    match output {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
                match kind {
                    "input_text" | "output_text" | "text" => {
                        Ok(part.get("text").and_then(Value::as_str).unwrap_or(""))
                    }
                    _ => Err(super::unsupported_part(kind)),
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|texts| texts.join("\n")),
        Some(other) => Ok(other.to_string()),
    }
}

/// Responses `tool_choice` → chat 形状：字符串取值相同；指名函数从扁平
/// `{"type":"function","name"}` 包成 `{"type":"function","function":{"name"}}`——原样透传时
/// 两跳到 Anthropic / Gemini 认不出，静默退成 auto，客户端指名的工具被放开。
/// 内置工具等降级路径表达不了的选择报错。
fn tool_choice_to_chat(choice: &Value) -> Result<Value, UpstreamError> {
    match choice {
        Value::String(_) => Ok(choice.clone()),
        Value::Object(obj) if obj.get("type").and_then(Value::as_str) == Some("function") => {
            let name = obj
                .get("name")
                .or_else(|| choice.pointer("/function/name"))
                .and_then(Value::as_str)
                .ok_or_else(|| super::unsupported_part("tool_choice"))?;
            Ok(json!({"type": "function", "function": {"name": name}}))
        }
        _ => Err(super::unsupported_part("tool_choice")),
    }
}

/// Responses 工具形状（扁平 name/parameters）→ chat function 工具。
fn convert_tools(
    src: &serde_json::Map<String, Value>,
    out: &mut serde_json::Map<String, Value>,
) -> Result<(), UpstreamError> {
    let tools: Vec<Value> = src
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            if t.get("type").and_then(Value::as_str) != Some("function") {
                return None; // 内置工具（web_search 等）降级路径不支持
            }
            Some(json!({"type": "function", "function": {
                "name": t.get("name").and_then(Value::as_str)?,
                "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                "parameters": t.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
            }}))
        })
        .collect();
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    if let Some(choice) = src.get("tool_choice").filter(|choice| !choice.is_null()) {
        out.insert("tool_choice".into(), tool_choice_to_chat(choice)?);
    }
    // chat 有同名开关；没有工具时上游会拒收它，所以只在带了工具时传
    if out.contains_key("tools")
        && let Some(parallel) = src
            .get("parallel_tool_calls")
            .filter(|value| value.is_boolean())
    {
        out.insert("parallel_tool_calls".into(), parallel.clone());
    }
    Ok(())
}

// ---- 响应转换（非流式） ----

/// Chat 响应 → Responses 对象。
pub fn response_chat_to_responses(
    body: &Bytes,
) -> Result<(Bytes, Option<UsageProbe>), UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let message = src
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or(Value::Null);

    let mut output: Vec<Value> = Vec::new();
    if let Some(reasoning) = reasoning_text(&message).filter(|text| !text.is_empty()) {
        output.push(json!({"type":"reasoning","id":"rs_0","status":"completed",
            "summary":[{"type":"summary_text","text":reasoning}]}));
    }
    let mut content: Vec<Value> = Vec::new();
    if let Some(text) = message.get("content").and_then(content_text)
        && !text.is_empty()
    {
        content.push(json!({"type": "output_text", "text": text, "annotations": []}));
    }
    // 上游拒答放在 message.refusal（content 为 null）；不带出来客户端就只看到空回复
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str)
        && !refusal.is_empty()
    {
        content.push(json!({"type": "refusal", "refusal": refusal}));
    }
    if !content.is_empty() {
        output.push(json!({
            "type": "message", "id": "msg_0", "status": "completed", "role": "assistant",
            "content": content,
        }));
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        output.push(json!({
            "type": "function_call",
            "id": format!("fc_{}", call.get("id").and_then(Value::as_str).unwrap_or("0")),
            "call_id": call.get("id").and_then(Value::as_str).unwrap_or(""),
            "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),
            "arguments": call.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}"),
            "status": "completed",
        }));
    }

    let usage: Option<UsageProbe> = src
        .get("usage")
        .filter(|u| !u.is_null())
        .and_then(|u| serde_json::from_value(u.clone()).ok());
    let probe = usage;
    let finish = src
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let (status, details) = response_completion(finish);
    let out = json!({
        "id": format!("resp_{}", src.get("id").and_then(Value::as_str).unwrap_or("0")),
        "object": "response",
        "created_at": src.get("created").and_then(Value::as_i64)
            .unwrap_or_else(|| chrono::Utc::now().timestamp()),
        "status": status,
        "incomplete_details": details,
        "model": src.get("model").and_then(Value::as_str).unwrap_or(""),
        "output": output,
        "usage": probe.map(responses_usage_json),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, probe))
}

/// chat 的 `content`：字符串，或分段数组（部分兼容上游这样回）。数组按顺序拼接其中的文本段
/// （`type` 缺省或为 `text` / `output_text`）；其余类型在 Responses 的输出文本里没有对应，跳过。
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                    None | Some("text" | "output_text") => part.get("text").and_then(Value::as_str),
                    Some(_) => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

fn response_completion(reason: Option<&str>) -> (&'static str, Option<Value>) {
    match reason {
        Some("length") => ("incomplete", Some(json!({"reason":"max_output_tokens"}))),
        Some("content_filter") => ("incomplete", Some(json!({"reason":"content_filter"}))),
        _ => ("completed", None),
    }
}

fn responses_usage_json(u: UsageProbe) -> Value {
    let Some(mut fields) = u.chat_json().as_object().cloned() else {
        return Value::Null;
    };
    for (from, to) in [
        ("prompt_tokens", "input_tokens"),
        ("completion_tokens", "output_tokens"),
        ("prompt_tokens_details", "input_tokens_details"),
        ("completion_tokens_details", "output_tokens_details"),
    ] {
        if let Some(value) = fields.remove(from) {
            fields.insert(to.into(), value);
        }
    }
    Value::Object(fields)
}

mod stream;
pub use stream::ChatStreamToResponses;
use stream::reasoning_text;

#[cfg(test)]
mod tests;
