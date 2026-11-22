//! Gemini 协议客户端（`models/{model}:generateContent`）→ OpenAI 方言上游：
//! 请求 Gemini→OpenAI chat（contents/systemInstruction/generationConfig/tools）；
//! 响应与 chunk 流 OpenAI→Gemini GenerateContentResponse，gateway 泵送与结算无感。
//! 与 `openai_to_gemini` 互为反向：那边服务 OpenAI 客户端 + Gemini 上游，
//! 这边服务 Gemini SDK / Gemini CLI 客户端 + OpenAI(兼容)/Anthropic 上游。
//!
//! usage 映射（反向对齐 `openai_to_gemini::usage_from_gemini`）：
//! promptTokenCount = prompt（含缓存）、cachedContentTokenCount = cached、
//! candidatesTokenCount = completion − reasoning、thoughtsTokenCount = reasoning。

use crate::error::UpstreamError;
use crate::reasoning::{RequestReasoning, parse_gemini_thinking_config};
use bytes::Bytes;
use okapi_api::UsageProbe;
use serde_json::{Value, json};

// ---- 请求转换 ----

/// Gemini generateContent 请求 → OpenAI chat 请求。模型名与流式标志来自 URL，由调用方传入。
pub fn request_gemini_to_openai(
    body: &Bytes,
    upstream_model: &str,
    stream: bool,
) -> Result<Bytes, UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let out = request_gemini_to_openai_value(&src, upstream_model, stream)?;
    serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 在已解析 `Value` 上完成 Gemini → OpenAI 请求转换（供单次解析管道直接复用）。
pub fn request_gemini_to_openai_value(
    src: &Value,
    upstream_model: &str,
    stream: bool,
) -> Result<Value, UpstreamError> {
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    let config = src
        .get("generationConfig")
        .or_else(|| src.get("generation_config"));
    super::single_choice(
        config.and_then(|c| c.get("candidateCount").or_else(|| c.get("candidate_count"))),
        "candidateCount",
    )?;

    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = src
        .get("systemInstruction")
        .or_else(|| src.get("system_instruction"))
    {
        let text = parts_text(system.get("parts"));
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }

    // Gemini 的 functionCall 没有 id，functionResponse 只靠 name 回指：
    // 转成 OpenAI 时按出现顺序生成 call_<n>，functionResponse 取同名最早未匹配的那个。
    let mut pending_calls: Vec<(String, String)> = Vec::new(); // (name, id)
    let mut call_seq = 0usize;
    for content in src
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if role == "model" {
            convert_model_parts(&parts, &mut messages, &mut pending_calls, &mut call_seq)?;
        } else {
            convert_user_parts(&parts, &mut messages, &mut pending_calls)?;
        }
    }

    let mut out = serde_json::Map::new();
    out.insert("model".into(), json!(upstream_model));
    out.insert("messages".into(), Value::Array(messages));
    if let Some(cfg) = src
        .get("generationConfig")
        .or_else(|| src.get("generation_config"))
        .and_then(Value::as_object)
    {
        apply_generation_config(cfg, &mut out);
    }
    if stream {
        out.insert("stream".into(), json!(true));
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }
    convert_tools(src, &mut out);

    Ok(Value::Object(out))
}

/// 只携带元数据、没有内容的 part 键（思维签名、媒体分辨率等）：跳过不算丢内容。
const PART_METADATA_KEYS: &[&str] = &[
    "thought",
    "thoughtSignature",
    "thought_signature",
    "videoMetadata",
    "video_metadata",
    "mediaResolution",
    "media_resolution",
    "partMetadata",
    "part_metadata",
];

/// 本角色认不出的内容 part（`executableCode` / `codeExecutionResult`、model 里的媒体……）
/// 报 400 而不是悄悄丢：丢掉代码执行历史，上游看到的是一段断了因果的对话。
fn reject_unhandled_part(part: &Value, handled: &[&str]) -> Result<(), UpstreamError> {
    let Some(obj) = part.as_object() else {
        return Ok(());
    };
    if handled.iter().any(|key| obj.contains_key(*key)) {
        return Ok(());
    }
    match obj
        .keys()
        .find(|key| !PART_METADATA_KEYS.contains(&key.as_str()))
    {
        Some(key) => Err(super::unsupported_part(&format!("part.{key}"))),
        None => Ok(()),
    }
}

/// `model` 角色：text → assistant content（thought 部件不回灌，OpenAI 无对应位）；
/// functionCall → tool_calls（同一条 content 内的多个调用并成一条 assistant 消息）。
/// 其余内容 part chat 的 assistant 消息装不下，见 [`reject_unhandled_part`]。
fn convert_model_parts(
    parts: &[Value],
    messages: &mut Vec<Value>,
    pending_calls: &mut Vec<(String, String)>,
    call_seq: &mut usize,
) -> Result<(), UpstreamError> {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for part in parts {
        reject_unhandled_part(part, &["text", "functionCall"])?;
        if let Some(t) = part.get("text").and_then(Value::as_str)
            && part.get("thought").and_then(Value::as_bool) != Some(true)
        {
            text.push_str(t);
        }
        if let Some(fc) = part.get("functionCall") {
            let name = fc.get("name").and_then(Value::as_str).unwrap_or("");
            let id = format!("call_{call_seq}");
            *call_seq += 1;
            pending_calls.push((name.to_owned(), id.clone()));
            tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": fc
                        .get("args")
                        .map_or_else(|| "{}".to_owned(), Value::to_string),
                }
            }));
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        return Ok(());
    }
    let mut msg = serde_json::Map::new();
    msg.insert("role".into(), json!("assistant"));
    msg.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !tool_calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    messages.push(Value::Object(msg));
    Ok(())
}

/// `user` 角色：functionResponse → tool 消息；其余部件（text / inlineData / fileData）
/// → 一条 user 消息（纯文本降成 string content，带媒体时用多段形状）。
/// **tool 消息先于这条 user 消息**：OpenAI 要求带 tool_calls 的 assistant 消息后面紧跟各个 tool 应答，
/// 中间插进 user 消息上游会整条 400；所以同一条 content 里文本排在 functionResponse 之前时，
/// 文本也要等 tool 应答之后再发。
/// chat 表达不了的媒体（视频、按 URI 引用的非图片文件）报错，见 [`super::unsupported_part`]。
fn convert_user_parts(
    parts: &[Value],
    messages: &mut Vec<Value>,
    pending_calls: &mut Vec<(String, String)>,
) -> Result<(), UpstreamError> {
    let mut content: Vec<Value> = Vec::new();
    for part in parts {
        reject_unhandled_part(
            part,
            &[
                "functionResponse",
                "text",
                "inlineData",
                "inline_data",
                "fileData",
                "file_data",
            ],
        )?;
        if let Some(fr) = part.get("functionResponse") {
            let name = fr.get("name").and_then(Value::as_str).unwrap_or("");
            let id = match pending_calls.iter().position(|(n, _)| n == name) {
                Some(pos) => pending_calls.remove(pos).1,
                None => format!("call_{name}"),
            };
            let response = fr.get("response").cloned().unwrap_or(Value::Null);
            // Gemini 惯例把结果包在 {"content"|"result"|"output": ...} 里；单键对象直接取值，
            // 让 OpenAI 上游看到的是结果本身而不是多一层壳
            let payload = match &response {
                Value::Object(o) if o.len() == 1 => {
                    o.values().next().cloned().unwrap_or(Value::Null)
                }
                other => other.clone(),
            };
            let text = match payload {
                Value::String(s) => s,
                Value::Null => String::new(),
                other => other.to_string(),
            };
            messages.push(json!({"role": "tool", "tool_call_id": id, "content": text}));
            continue;
        }
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            content.push(json!({"type": "text", "text": t}));
        } else if let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data")) {
            let mime = inline
                .get("mimeType")
                .or_else(|| inline.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            let data = inline.get("data").and_then(Value::as_str).unwrap_or("");
            if mime.starts_with("image/") {
                content.push(json!({"type": "image_url",
                    "image_url": {"url": format!("data:{mime};base64,{data}")}}));
            } else if let Some(format) = mime.strip_prefix("audio/") {
                content.push(json!({"type": "input_audio",
                    "input_audio": {"data": data, "format": format}}));
            } else if mime == "application/pdf" {
                content.push(json!({"type": "file",
                    "file": {"file_data": format!("data:{mime};base64,{data}")}}));
            } else {
                return Err(super::unsupported_part(mime));
            }
        } else if let Some(file) = part.get("fileData").or_else(|| part.get("file_data")) {
            let mime = file
                .get("mimeType")
                .or_else(|| file.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(uri) = file
                .get("fileUri")
                .or_else(|| file.get("file_uri"))
                .and_then(Value::as_str)
                .filter(|_| mime.starts_with("image/"))
            else {
                return Err(super::unsupported_part("fileData"));
            };
            content.push(json!({"type": "image_url", "image_url": {"url": uri}}));
        }
    }
    flush_user(&mut content, messages);
    Ok(())
}

fn flush_user(content: &mut Vec<Value>, messages: &mut Vec<Value>) {
    if content.is_empty() {
        return;
    }
    let mut taken = std::mem::take(content);
    let only_text = taken.len() == 1 && taken[0].get("type") == Some(&json!("text"));
    let value = if only_text {
        taken[0].get_mut("text").map_or(Value::Null, Value::take)
    } else {
        Value::Array(taken)
    };
    messages.push(json!({"role": "user", "content": value}));
}

fn parts_text(parts: Option<&Value>) -> String {
    parts
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_generation_config(
    cfg: &serde_json::Map<String, Value>,
    out: &mut serde_json::Map<String, Value>,
) {
    let pick = |camel: &str, snake: &str| cfg.get(camel).or_else(|| cfg.get(snake)).cloned();
    if let Some(v) = pick("maxOutputTokens", "max_output_tokens") {
        out.insert("max_tokens".into(), v);
    }
    if let Some(v) = cfg.get("temperature") {
        out.insert("temperature".into(), v.clone());
    }
    if let Some(v) = pick("topP", "top_p") {
        out.insert("top_p".into(), v);
    }
    if let Some(v) = pick("stopSequences", "stop_sequences") {
        out.insert("stop".into(), v);
    }
    if let Some(v) = pick("presencePenalty", "presence_penalty") {
        out.insert("presence_penalty".into(), v);
    }
    if let Some(v) = pick("frequencyPenalty", "frequency_penalty") {
        out.insert("frequency_penalty".into(), v);
    }
    if let Some(n) = cfg.get("seed") {
        out.insert("seed".into(), n.clone());
    }
    // 结构化输出：responseSchema 优先（json_schema），只给 mimeType 时退 json_object
    let schema = pick("responseSchema", "response_schema")
        .or_else(|| pick("responseJsonSchema", "response_json_schema"));
    let mime = pick("responseMimeType", "response_mime_type");
    if let Some(schema) = schema {
        out.insert(
            "response_format".into(),
            json!({"type": "json_schema", "json_schema": {"name": "response", "schema": schema}}),
        );
    } else if mime.as_ref().and_then(Value::as_str) == Some("application/json") {
        out.insert("response_format".into(), json!({"type": "json_object"}));
    }
    // thinkingConfig（thinkingBudget / Gemini 3 thinkingLevel）→ reasoning_effort 档位。
    // 预算 0 或 thinkingLevel=none/off 为关闭思考：OpenAI 侧不注入。
    if let Some(tc) = pick("thinkingConfig", "thinking_config")
        .as_ref()
        .and_then(Value::as_object)
        && let RequestReasoning::Enabled(directive) = parse_gemini_thinking_config(tc)
    {
        out.insert(
            "reasoning_effort".into(),
            json!(directive.effective_effort().as_str()),
        );
    }
}

/// `tools[].functionDeclarations` → OpenAI function 工具；内置工具（googleSearch /
/// codeExecution / urlContext）OpenAI chat 无对应，丢弃。toolConfig → tool_choice。
fn convert_tools(src: &serde_json::Map<String, Value>, out: &mut serde_json::Map<String, Value>) {
    let decls: Vec<Value> = src
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            t.get("functionDeclarations")
                .or_else(|| t.get("function_declarations"))
                .and_then(Value::as_array)
        })
        .flatten()
        .filter_map(|f| {
            Some(json!({"type": "function", "function": {
                "name": f.get("name").and_then(Value::as_str)?,
                "description": f.get("description").and_then(Value::as_str).unwrap_or(""),
                "parameters": f.get("parameters")
                    .or_else(|| f.get("parametersJsonSchema"))
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            }}))
        })
        .collect();
    if decls.is_empty() {
        return;
    }
    out.insert("tools".into(), Value::Array(decls));
    let calling = src
        .get("toolConfig")
        .or_else(|| src.get("tool_config"))
        .and_then(|c| {
            c.get("functionCallingConfig")
                .or_else(|| c.get("function_calling_config"))
        });
    let Some(calling) = calling else {
        return;
    };
    let allowed = calling
        .get("allowedFunctionNames")
        .or_else(|| calling.get("allowed_function_names"))
        .and_then(Value::as_array);
    let choice = match calling.get("mode").and_then(Value::as_str) {
        Some("NONE") => Some(json!("none")),
        Some("ANY") => match allowed {
            Some(names) if names.len() == 1 => names[0]
                .as_str()
                .map(|n| json!({"type": "function", "function": {"name": n}})),
            _ => Some(json!("required")),
        },
        Some("AUTO") => Some(json!("auto")),
        _ => None,
    };
    if let Some(choice) = choice {
        out.insert("tool_choice".into(), choice);
    }
}

// ---- 响应转换（非流式） ----

/// OpenAI chat.completion → Gemini GenerateContentResponse。
pub fn response_openai_to_gemini(
    body: &Bytes,
) -> Result<(Bytes, Option<UsageProbe>), UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let message = src
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or(Value::Null);

    let mut parts: Vec<Value> = Vec::new();
    if let Some(reasoning) = super::chat_reasoning_text(&message) {
        parts.push(json!({"text": reasoning, "thought": true}));
    }
    // 上游拒答在 message.refusal（content 为 null）；Gemini 没有拒答字段，按正文给出
    for field in ["content", "refusal"] {
        if let Some(text) = message.get(field).and_then(Value::as_str)
            && !text.is_empty()
        {
            parts.push(json!({"text": text}));
        }
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        parts.push(function_call_part(
            call.pointer("/function/name").and_then(Value::as_str),
            call.pointer("/function/arguments").and_then(Value::as_str),
        ));
    }

    let finish = src
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let usage = okapi_api::usage_from_chat(&src);
    let probe = usage;
    let out = json!({
        "candidates": [{
            "content": {"parts": parts, "role": "model"},
            "finishReason": map_finish(finish),
            "index": 0,
        }],
        "usageMetadata": probe.map(gemini_usage_json),
        "modelVersion": src.get("model").and_then(Value::as_str).unwrap_or(""),
        "responseId": src.get("id").and_then(Value::as_str).unwrap_or(""),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, probe))
}

fn function_call_part(name: Option<&str>, arguments: Option<&str>) -> Value {
    let args = arguments
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    json!({"functionCall": {"name": name.unwrap_or(""), "args": args}})
}

fn map_finish(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "MAX_TOKENS",
        Some("content_filter") => "SAFETY",
        // tool_calls / stop / 缺失：Gemini 对函数调用也回 STOP
        _ => "STOP",
    }
}

/// OpenAI 口径探针 → Gemini usageMetadata（candidates 不含 thoughts，total 三者相加）。
#[must_use]
pub fn gemini_usage_json(u: UsageProbe) -> Value {
    if u.with_estimates(0, 0).is_err() {
        return Value::Null;
    }
    let thoughts = u.completion_tokens_details.reasoning_tokens;
    let candidates = u.completion_tokens.saturating_sub(thoughts);
    let mut meta = json!({
        "promptTokenCount": u.prompt_tokens,
        "candidatesTokenCount": candidates,
        "totalTokenCount": u64::from(u.prompt_tokens) + u64::from(u.completion_tokens),
    });
    if let Some(fields) = meta.as_object_mut() {
        if u.missing_prompt {
            fields.remove("promptTokenCount");
        }
        if u.missing_completion {
            fields.remove("candidatesTokenCount");
        }
        if u.missing_prompt || u.missing_completion {
            fields.remove("totalTokenCount");
        }
    }
    if u.prompt_tokens_details.cache_read_reported || u.prompt_tokens_details.cached_tokens > 0 {
        meta["cachedContentTokenCount"] = json!(u.prompt_tokens_details.cached_tokens);
    }
    if output_reasoning_reported(u) {
        meta["thoughtsTokenCount"] = json!(thoughts);
    }
    let input = u.prompt_tokens_details;
    let output = u.completion_tokens_details;
    if let Some(details) = modal_counts(
        (!u.missing_prompt).then_some(u.prompt_tokens),
        (input.modalities_reported.audio || input.audio_tokens > 0).then_some(input.audio_tokens),
        (input.modalities_reported.image || input.image_tokens > 0).then_some(input.image_tokens),
    ) {
        meta["promptTokensDetails"] = details;
    }
    if let Some(cache) = input.cached_tokens_details {
        let reported = cache.reported(input.cached_tokens);
        // Gemini requires complete cache composition. Omit an incomplete split
        // instead of inventing a measured zero for an unreported modality.
        if reported.audio && reported.image {
            meta["cacheTokensDetails"] = modal_counts(
                Some(input.cached_tokens),
                Some(cache.audio_tokens.unwrap_or(0)),
                Some(cache.image_tokens.unwrap_or(0)),
            )
            .unwrap_or(Value::Null);
        }
    }
    if let Some(details) = modal_counts(
        (!u.missing_completion).then_some(candidates),
        (output.modalities_reported.audio || output.audio_tokens > 0)
            .then_some(output.audio_tokens),
        (output.modalities_reported.image || output.image_tokens > 0)
            .then_some(output.image_tokens),
    ) {
        meta["candidatesTokensDetails"] = details;
    }
    meta
}

fn output_reasoning_reported(u: UsageProbe) -> bool {
    u.completion_tokens_details.reasoning_reported
        || u.completion_tokens_details.reasoning_tokens > 0
}

fn modal_counts(total: Option<u32>, audio: Option<u32>, image: Option<u32>) -> Option<Value> {
    if audio.is_none() && image.is_none() {
        return None;
    }
    let mut rows = Vec::new();
    if let (Some(total), Some(audio), Some(image)) = (total, audio, image) {
        let text = total.checked_sub(audio)?.checked_sub(image)?;
        rows.push(json!({"modality":"TEXT","tokenCount":text}));
    }
    if let Some(audio) = audio {
        rows.push(json!({"modality":"AUDIO","tokenCount":audio}));
    }
    if let Some(image) = image {
        rows.push(json!({"modality":"IMAGE","tokenCount":image}));
    }
    Some(Value::Array(rows))
}

mod stream;
pub use stream::{OaiStreamToGemini, wrap_chat_as_gemini};

#[cfg(test)]
mod tests;
