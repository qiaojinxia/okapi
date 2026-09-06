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
use crate::openai::{ChatResponse, StreamHandle};
use crate::reasoning::ReasoningDirective;
use crate::types::ChatEvent;
use bytes::Bytes;
use futures::StreamExt;
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
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };

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
            convert_model_parts(&parts, &mut messages, &mut pending_calls, &mut call_seq);
        } else {
            convert_user_parts(&parts, &mut messages, &mut pending_calls);
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

    serde_json::to_vec(&Value::Object(out))
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// `model` 角色：text → assistant content（thought 部件不回灌，OpenAI 无对应位）；
/// functionCall → tool_calls（同一条 content 内的多个调用并成一条 assistant 消息）。
fn convert_model_parts(
    parts: &[Value],
    messages: &mut Vec<Value>,
    pending_calls: &mut Vec<(String, String)>,
    call_seq: &mut usize,
) {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for part in parts {
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
        return;
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
}

/// `user` 角色：functionResponse → tool 消息；其余部件（text / inlineData / fileData）
/// → 一条 user 消息（纯文本降成 string content，带媒体时用多段形状）。
fn convert_user_parts(
    parts: &[Value],
    messages: &mut Vec<Value>,
    pending_calls: &mut Vec<(String, String)>,
) {
    let mut content: Vec<Value> = Vec::new();
    for part in parts {
        if let Some(fr) = part.get("functionResponse") {
            flush_user(&mut content, messages);
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
            }
            // 其它 mime（pdf/video）OpenAI chat 无对应部件：丢弃
        } else if let Some(file) = part.get("fileData").or_else(|| part.get("file_data")) {
            let mime = file
                .get("mimeType")
                .or_else(|| file.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(uri) = file
                .get("fileUri")
                .or_else(|| file.get("file_uri"))
                .and_then(Value::as_str)
                && mime.starts_with("image/")
            {
                content.push(json!({"type": "image_url", "image_url": {"url": uri}}));
            }
        }
    }
    flush_user(&mut content, messages);
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
    // thinkingConfig.thinkingBudget → reasoning_effort 档位（OpenAI 没有预算参数）。
    // 预算 0 = 关闭思考：OpenAI 侧不注入；includeThoughts 无对应（推理摘要由上游决定）。
    if let Some(budget) = pick("thinkingConfig", "thinking_config")
        .as_ref()
        .and_then(|t| t.get("thinkingBudget").or_else(|| t.get("thinking_budget")))
        .and_then(Value::as_u64)
        .and_then(|b| u32::try_from(b).ok())
        .filter(|b| *b > 0)
    {
        let directive = ReasoningDirective {
            effort: None,
            budget_tokens: Some(budget),
        };
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
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        parts.push(json!({"text": reasoning, "thought": true}));
    }
    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        parts.push(json!({"text": text}));
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
    let usage: Option<UsageProbe> = src
        .get("usage")
        .and_then(|u| serde_json::from_value(u.clone()).ok());
    let probe = usage.unwrap_or_default();
    let out = json!({
        "candidates": [{
            "content": {"parts": parts, "role": "model"},
            "finishReason": map_finish(finish),
            "index": 0,
        }],
        "usageMetadata": gemini_usage_json(probe),
        "modelVersion": src.get("model").and_then(Value::as_str).unwrap_or(""),
        "responseId": src.get("id").and_then(Value::as_str).unwrap_or(""),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, Some(probe)))
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
    let thoughts = u
        .completion_tokens_details
        .reasoning_tokens
        .min(u.completion_tokens);
    let candidates = u.completion_tokens - thoughts;
    let mut meta = json!({
        "promptTokenCount": u.prompt_tokens,
        "candidatesTokenCount": candidates,
        "totalTokenCount": u.prompt_tokens.saturating_add(u.completion_tokens),
    });
    if u.prompt_tokens_details.cached_tokens > 0 {
        meta["cachedContentTokenCount"] = json!(u.prompt_tokens_details.cached_tokens);
    }
    if thoughts > 0 {
        meta["thoughtsTokenCount"] = json!(thoughts);
    }
    meta
}

// ---- 事件流转换 ----

/// 累积中的工具调用（OpenAI 按 index 分片送 arguments；Gemini 要一次给完整 functionCall）。
#[derive(Default)]
struct PendingCall {
    name: String,
    arguments: String,
}

/// OpenAI chunk 流 → Gemini SSE chunk 流的有状态转换器。
/// 文本 / thought 逐 chunk 直出；functionCall 在终局一次给出（参数需要拼完整）；
/// 终局 chunk 带 finishReason + usageMetadata；Gemini 无 [DONE]。
pub struct OaiStreamToGemini {
    model: String,
    id: String,
    calls: Vec<PendingCall>,
    finish_reason: Option<String>,
    usage: Option<UsageProbe>,
    finished: bool,
}

impl OaiStreamToGemini {
    #[must_use]
    pub fn new(fallback_model: &str) -> Self {
        Self {
            model: fallback_model.to_owned(),
            id: String::new(),
            calls: Vec::new(),
            finish_reason: None,
            usage: None,
            finished: false,
        }
    }

    pub fn step(
        &mut self,
        item: Result<ChatEvent, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        match item {
            Err(err) => vec![Err(err)],
            Ok(ChatEvent::Done) => self.finish(),
            Ok(ChatEvent::Data { raw, usage, .. }) => {
                if let Some(u) = usage {
                    self.usage = Some(u);
                }
                let chunk: Value = serde_json::from_str(&raw).unwrap_or_default();
                self.on_chunk(&chunk)
            }
        }
    }

    fn on_chunk(&mut self, chunk: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        if let Some(model) = chunk.get("model").and_then(Value::as_str)
            && !model.is_empty()
        {
            model.clone_into(&mut self.model);
        }
        if self.id.is_empty()
            && let Some(id) = chunk.get("id").and_then(Value::as_str)
        {
            id.clone_into(&mut self.id);
        }
        let mut out = Vec::new();
        let delta = chunk
            .pointer("/choices/0/delta")
            .cloned()
            .unwrap_or_default();

        if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push(Ok(self.chunk(
                &[json!({"text": text, "thought": true})],
                None,
                None,
                text.chars().count(),
            )));
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push(Ok(self.chunk(
                &[json!({"text": text})],
                None,
                None,
                text.chars().count(),
            )));
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| usize::try_from(i).ok())
                .unwrap_or(self.calls.len().saturating_sub(1));
            while self.calls.len() <= index {
                self.calls.push(PendingCall::default());
            }
            let slot = &mut self.calls[index];
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                slot.name.push_str(name);
            }
            if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                slot.arguments.push_str(args);
            }
        }
        if let Some(finish) = chunk
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            self.finish_reason = Some(finish.to_owned());
        }
        out
    }

    /// 终局：拼完的 functionCall（若有）+ finishReason + usageMetadata，再 Done。
    fn finish(&mut self) -> Vec<Result<ChatEvent, UpstreamError>> {
        if self.finished {
            return vec![Ok(ChatEvent::Done)];
        }
        self.finished = true;
        let calls = std::mem::take(&mut self.calls);
        let parts: Vec<Value> = calls
            .iter()
            .filter(|c| !c.name.is_empty() || !c.arguments.is_empty())
            .map(|c| function_call_part(Some(&c.name), Some(&c.arguments)))
            .collect();
        let has_calls = !parts.is_empty();
        let probe = self.usage.unwrap_or_default();
        let finish = map_finish(self.finish_reason.as_deref());
        let chars: usize = calls.iter().map(|c| c.arguments.chars().count()).sum();
        let mut last = self.chunk(&parts, Some(finish), Some(probe), chars);
        if !has_calls && let ChatEvent::Data { has_output, .. } = &mut last {
            *has_output = false;
        }
        vec![Ok(last), Ok(ChatEvent::Done)]
    }

    fn chunk(
        &self,
        parts: &[Value],
        finish_reason: Option<&str>,
        usage: Option<UsageProbe>,
        content_chars: usize,
    ) -> ChatEvent {
        let mut candidate = json!({"content": {"parts": parts, "role": "model"}, "index": 0});
        if let Some(f) = finish_reason {
            candidate["finishReason"] = json!(f);
        }
        let mut body = json!({
            "candidates": [candidate],
            "modelVersion": self.model,
        });
        if !self.id.is_empty() {
            body["responseId"] = json!(self.id);
        }
        if let Some(u) = usage {
            body["usageMetadata"] = gemini_usage_json(u);
        }
        ChatEvent::Data {
            raw: body.to_string(),
            event: None,
            has_output: true,
            content_chars,
            usage,
        }
    }
}

/// chat 形状（JSON / chunk 流）→ Gemini 形状：gateway 在 OpenAI(兼容) / Anthropic
/// 上游的回程用它包一层，泵送与结算照旧读 usage。
pub fn wrap_chat_as_gemini(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            ..
        } => {
            let (body, usage) = response_openai_to_gemini(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            })
        }
        ChatResponse::Stream(h) => {
            let mut st = OaiStreamToGemini::new(upstream_model);
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

    fn req(v: &Value) -> Value {
        let out = request_gemini_to_openai(&Bytes::from(v.to_string()), "gpt-x", false).unwrap();
        serde_json::from_slice(&out).unwrap()
    }

    #[test]
    fn maps_system_contents_and_config() {
        let v = req(&json!({
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"text": "hello"}, {"text": "hidden", "thought": true}]},
                {"role": "user", "parts": [{"text": "look"},
                    {"inlineData": {"mimeType": "image/png", "data": "AAAA"}}]}
            ],
            "generationConfig": {"maxOutputTokens": 64, "temperature": 0.2, "topP": 0.9,
                "stopSequences": ["END"], "responseMimeType": "application/json",
                "thinkingConfig": {"thinkingBudget": 2048}}
        }));
        assert_eq!(v["model"], "gpt-x");
        assert_eq!(
            v["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(v["messages"][1], json!({"role": "user", "content": "hi"}));
        assert_eq!(
            v["messages"][2],
            json!({"role": "assistant", "content": "hello"}),
            "thought 部件不回灌"
        );
        assert_eq!(v["messages"][3]["content"][1]["type"], "image_url");
        assert_eq!(
            v["messages"][3]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(v["max_tokens"], 64);
        assert_eq!(v["temperature"], 0.2);
        assert_eq!(v["top_p"], 0.9);
        assert_eq!(v["stop"], json!(["END"]));
        assert_eq!(v["response_format"], json!({"type": "json_object"}));
        assert!(v["reasoning_effort"].is_string(), "thinkingBudget 折成档位");
        assert!(v.get("stream").is_none());
    }

    #[test]
    fn maps_tools_and_function_round_trip() {
        let v = req(&json!({
            "contents": [
                {"role": "user", "parts": [{"text": "weather?"}]},
                {"role": "model", "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "SF"}}}]},
                {"role": "user", "parts": [{"functionResponse": {"name": "get_weather",
                    "response": {"content": "sunny"}}}]}
            ],
            "tools": [{"functionDeclarations": [{"name": "get_weather", "description": "d",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["get_weather"]}}
        }));
        assert_eq!(v["messages"][1]["role"], "assistant");
        assert_eq!(v["messages"][1]["content"], Value::Null);
        assert_eq!(v["messages"][1]["tool_calls"][0]["id"], "call_0");
        assert_eq!(
            v["messages"][1]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            v["messages"][1]["tool_calls"][0]["function"]["arguments"],
            r#"{"city":"SF"}"#
        );
        assert_eq!(
            v["messages"][2],
            json!({"role": "tool", "tool_call_id": "call_0", "content": "sunny"}),
            "functionResponse 按名字回指到 call_0"
        );
        assert_eq!(v["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(
            v["tool_choice"],
            json!({"type": "function", "function": {"name": "get_weather"}})
        );
    }

    #[test]
    fn stream_flag_adds_usage_option() {
        let out = request_gemini_to_openai(
            &Bytes::from(json!({"contents": [{"parts": [{"text": "x"}]}]}).to_string()),
            "m",
            true,
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["stream"], true);
        assert_eq!(v["stream_options"], json!({"include_usage": true}));
        assert_eq!(v["messages"][0]["role"], "user", "缺 role 视为 user");
    }

    #[test]
    fn response_maps_to_gemini_shape() {
        let body = json!({
            "id": "chatcmpl-1", "model": "gpt-real",
            "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": "ok", "reasoning_content": "hmm",
                "tool_calls": [{"id": "c1", "type": "function",
                    "function": {"name": "f", "arguments": "{\"a\":1}"}}]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 7,
                "prompt_tokens_details": {"cached_tokens": 4},
                "completion_tokens_details": {"reasoning_tokens": 3}}
        });
        let (out, usage) = response_openai_to_gemini(&Bytes::from(body.to_string())).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let parts = &v["candidates"][0]["content"]["parts"];
        assert_eq!(parts[0], json!({"text": "hmm", "thought": true}));
        assert_eq!(parts[1], json!({"text": "ok"}));
        assert_eq!(
            parts[2],
            json!({"functionCall": {"name": "f", "args": {"a": 1}}})
        );
        assert_eq!(v["candidates"][0]["finishReason"], "STOP");
        assert_eq!(v["candidates"][0]["content"]["role"], "model");
        assert_eq!(v["modelVersion"], "gpt-real");
        assert_eq!(
            v["usageMetadata"],
            json!({"promptTokenCount": 10, "candidatesTokenCount": 4, "totalTokenCount": 17,
                   "cachedContentTokenCount": 4, "thoughtsTokenCount": 3})
        );
        let u = usage.unwrap();
        assert_eq!((u.prompt_tokens, u.completion_tokens), (10, 7));
    }

    #[allow(clippy::unnecessary_wraps)] // 与 step() 的输入类型对齐
    fn data(raw: &Value) -> Result<ChatEvent, UpstreamError> {
        Ok(ChatEvent::Data {
            raw: raw.to_string(),
            event: None,
            has_output: true,
            content_chars: 0,
            usage: raw
                .get("usage")
                .and_then(|u| serde_json::from_value(u.clone()).ok()),
        })
    }

    #[test]
    fn stream_emits_text_then_final_chunk_with_calls_and_usage() {
        let mut st = OaiStreamToGemini::new("m");
        let mut out = Vec::new();
        out.extend(st.step(data(&json!({"id": "c1", "model": "gpt-real",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}))));
        out.extend(st.step(data(
            &json!({"choices": [{"index": 0, "delta": {"content": "lo"}}]}),
        )));
        out.extend(st.step(data(&json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "c", "type": "function", "function": {"name": "f", "arguments": "{\"a\""}}]}}]}))));
        out.extend(st.step(data(
            &json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": ":1}"}}]}}]}),
        )));
        out.extend(st.step(data(
            &json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        )));
        out.extend(st.step(data(&json!({"choices": [],
            "usage": {"prompt_tokens": 5, "completion_tokens": 9}}))));
        out.extend(st.step(Ok(ChatEvent::Done)));

        let datas: Vec<Value> = out
            .iter()
            .filter_map(|e| match e {
                Ok(ChatEvent::Data { raw, .. }) => serde_json::from_str(raw).ok(),
                _ => None,
            })
            .collect();
        assert_eq!(datas.len(), 3, "两段文本 + 一条终局：{datas:?}");
        assert_eq!(
            datas[0]["candidates"][0]["content"]["parts"][0]["text"],
            "Hel"
        );
        assert_eq!(datas[0]["modelVersion"], "gpt-real");
        assert_eq!(datas[0]["responseId"], "c1");
        assert_eq!(
            datas[1]["candidates"][0]["content"]["parts"][0]["text"],
            "lo"
        );
        let last = &datas[2];
        assert_eq!(
            last["candidates"][0]["content"]["parts"][0],
            json!({"functionCall": {"name": "f", "args": {"a": 1}}}),
            "分片参数拼完整后一次给出"
        );
        assert_eq!(last["candidates"][0]["finishReason"], "STOP");
        assert_eq!(last["usageMetadata"]["promptTokenCount"], 5);
        assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 9);
        assert!(matches!(out.last(), Some(Ok(ChatEvent::Done))));
        let usage_events = out
            .iter()
            .filter(|e| matches!(e, Ok(ChatEvent::Data { usage: Some(_), .. })))
            .count();
        assert_eq!(usage_events, 1, "usage 只随终局 chunk 给结算");
    }

    #[test]
    fn stream_final_chunk_without_output_is_not_output() {
        let mut st = OaiStreamToGemini::new("m");
        let out = st.step(Ok(ChatEvent::Done));
        assert_eq!(out.len(), 2);
        assert!(matches!(
            out[0],
            Ok(ChatEvent::Data {
                has_output: false,
                ..
            })
        ));
    }
}
