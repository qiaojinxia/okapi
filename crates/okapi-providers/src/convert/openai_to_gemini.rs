//! OpenAI 协议客户端 → Gemini 上游：
//! 请求 OpenAI→Gemini（contents/systemInstruction/generationConfig/tools）；
//! 响应与 chunk 流 Gemini→OpenAI，gateway 泵送与结算无感。
//!
//! usage 映射：promptTokenCount 已含 cachedContentTokenCount → prompt 直取、
//! cached = cachedContentTokenCount；completion = candidates + thoughts、
//! reasoning = thoughtsTokenCount。

use crate::error::UpstreamError;
use crate::gemini::{GeminiResponse, GeminiUpstream};
use crate::openai::{ChatResponse, StreamHandle};
use crate::types::ChatEvent;
use bytes::Bytes;
use futures::StreamExt;
use okapi_api::{CompletionTokensDetails, PromptTokensDetails, UsageProbe};
use serde_json::{Value, json};
use std::collections::HashMap;

// ---- 请求转换 ----

/// OpenAI chat 请求 → Gemini generateContent 请求（模型名走 URL，不在 body）。
pub fn request_openai_to_gemini(body: &Bytes) -> Result<Bytes, UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };

    let call_names = collect_call_names(src);

    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    for msg in src
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = msg.get("content").unwrap_or(&Value::Null);
        match role {
            "system" | "developer" => {
                let text = plain_text(content);
                if !text.is_empty() {
                    system_parts.push(json!({"text": text}));
                }
            }
            "assistant" => {
                let mut parts = content_to_parts(content);
                for call in msg
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let args = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or_else(|| json!({}));
                    parts.push(json!({"functionCall": {"name": name, "args": args}}));
                }
                if !parts.is_empty() {
                    push_content(&mut contents, "model", parts);
                }
            }
            "tool" => {
                let id = msg
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = call_names.get(id).map_or("", String::as_str);
                let part = json!({"functionResponse": {
                    "name": name,
                    "response": {"content": plain_text(content)},
                }});
                push_content(&mut contents, "user", vec![part]);
            }
            _ => {
                let parts = content_to_parts(content);
                if !parts.is_empty() {
                    push_content(&mut contents, "user", parts);
                }
            }
        }
    }

    let mut out = serde_json::Map::new();
    if !system_parts.is_empty() {
        out.insert("systemInstruction".into(), json!({"parts": system_parts}));
    }
    out.insert("contents".into(), Value::Array(contents));

    if let Some(cfg) = generation_config(src) {
        out.insert("generationConfig".into(), cfg);
    }

    convert_tools(src, &mut out);

    serde_json::to_vec(&Value::Object(out))
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// tool_call_id → 函数名（functionResponse 必须带 name）。
fn collect_call_names(src: &serde_json::Map<String, Value>) -> HashMap<String, String> {
    let mut call_names = HashMap::new();
    for msg in src
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for call in msg
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(id), Some(name)) = (
                call.get("id").and_then(Value::as_str),
                call.pointer("/function/name").and_then(Value::as_str),
            ) {
                call_names.insert(id.to_owned(), name.to_owned());
            }
        }
    }
    call_names
}

fn generation_config(src: &serde_json::Map<String, Value>) -> Option<Value> {
    let mut cfg = serde_json::Map::new();
    if let Some(v) = src
        .get("max_completion_tokens")
        .or_else(|| src.get("max_tokens"))
    {
        cfg.insert("maxOutputTokens".into(), v.clone());
    }
    if let Some(v) = src.get("temperature") {
        cfg.insert("temperature".into(), v.clone());
    }
    if let Some(v) = src.get("top_p") {
        cfg.insert("topP".into(), v.clone());
    }
    match src.get("stop") {
        Some(Value::String(s)) => {
            cfg.insert("stopSequences".into(), json!([s]));
        }
        Some(Value::Array(a)) => {
            cfg.insert("stopSequences".into(), Value::Array(a.clone()));
        }
        _ => {}
    }
    (!cfg.is_empty()).then_some(Value::Object(cfg))
}

/// 连续同角色合并（Gemini 要求 user/model 交替）。
fn push_content(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if let Some(last) = contents.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut)
    {
        existing.extend(parts);
        return;
    }
    contents.push(json!({"role": role, "parts": parts}));
}

fn content_to_parts(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) if !s.is_empty() => vec![json!({"text": s})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|t| json!({"text": t})),
                Some("image_url") => {
                    let url = p.get("image_url")?.get("url").and_then(Value::as_str)?;
                    let rest = url.strip_prefix("data:")?;
                    let (mime, data) = rest.split_once(";base64,")?;
                    Some(json!({"inlineData": {"mimeType": mime, "data": data}}))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn plain_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn convert_tools(src: &serde_json::Map<String, Value>, out: &mut serde_json::Map<String, Value>) {
    let choice = src.get("tool_choice");
    if choice.and_then(Value::as_str) == Some("none") {
        return;
    }
    let decls: Vec<Value> = src
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(json!({
                "name": f.get("name").and_then(Value::as_str)?,
                "description": f.get("description").and_then(Value::as_str).unwrap_or(""),
                "parameters": f.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
            }))
        })
        .collect();
    if decls.is_empty() {
        return;
    }
    out.insert("tools".into(), json!([{"functionDeclarations": decls}]));
    let mode = match choice {
        Some(Value::String(s)) if s == "required" => Some(json!({"mode": "ANY"})),
        Some(Value::Object(o)) => o
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .map(|name| json!({"mode": "ANY", "allowedFunctionNames": [name]})),
        _ => None,
    };
    if let Some(cfg) = mode {
        out.insert("toolConfig".into(), json!({"functionCallingConfig": cfg}));
    }
}

// ---- 响应转换（非流式） ----

/// Gemini 响应 → OpenAI chat.completion（含 usage 探针）。
pub fn response_gemini_to_openai(
    body: &Bytes,
    model: &str,
) -> Result<(Bytes, Option<UsageProbe>), UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let (text, reasoning, tool_calls) = collect_parts(&src);

    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls.clone()));
    }

    let finish = src
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str);
    let usage = usage_from_gemini(src.get("usageMetadata"));
    let out = json!({
        "id": src.get("responseId").and_then(Value::as_str).unwrap_or("gemini"),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": map_finish(finish, !tool_calls.is_empty()),
        }],
        "usage": usage_json(usage),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, Some(usage)))
}

/// candidates[0].content.parts → (可见文本, thought 文本, tool_calls)。
fn collect_parts(src: &Value) -> (String, String, Vec<Value>) {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for part in src
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                reasoning.push_str(t);
            } else {
                text.push_str(t);
            }
        }
        if let Some(fc) = part.get("functionCall") {
            let index = tool_calls.len();
            tool_calls.push(json!({
                "index": index,
                "id": format!("call_{index}"),
                "type": "function",
                "function": {
                    "name": fc.get("name").and_then(Value::as_str).unwrap_or(""),
                    "arguments": fc
                        .get("args")
                        .map_or_else(|| "{}".to_owned(), std::string::ToString::to_string),
                }
            }));
        }
    }
    (text, reasoning, tool_calls)
}

fn map_finish(finish: Option<&str>, has_tools: bool) -> &'static str {
    if has_tools {
        return "tool_calls";
    }
    match finish {
        Some("MAX_TOKENS") => "length",
        Some("SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT") => "content_filter",
        _ => "stop",
    }
}

fn usage_from_gemini(meta: Option<&Value>) -> UsageProbe {
    let get = |k: &str| {
        meta.and_then(|m| m.get(k))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0)
    };
    let thoughts = get("thoughtsTokenCount");
    UsageProbe {
        // promptTokenCount 已含 cachedContentTokenCount
        prompt_tokens: get("promptTokenCount"),
        completion_tokens: get("candidatesTokenCount").saturating_add(thoughts),
        prompt_tokens_details: PromptTokensDetails {
            cached_tokens: get("cachedContentTokenCount"),
            // Gemini 显式缓存的创建走独立的 cachedContents API 计费，不在生成响应的 usage 里
            cache_write_tokens: 0,
            // Gemini 的 promptTokensDetails 是带 modality 的数组，解析待接入
            audio_tokens: 0,
            image_tokens: 0,
        },
        completion_tokens_details: CompletionTokensDetails {
            reasoning_tokens: thoughts,
            audio_tokens: 0,
        },
    }
}

fn usage_json(u: UsageProbe) -> Value {
    json!({
        "prompt_tokens": u.prompt_tokens,
        "completion_tokens": u.completion_tokens,
        "total_tokens": u.prompt_tokens + u.completion_tokens,
        "prompt_tokens_details": {"cached_tokens": u.prompt_tokens_details.cached_tokens},
        "completion_tokens_details": {"reasoning_tokens": u.completion_tokens_details.reasoning_tokens},
    })
}

// ---- 流式转换 ----

/// Gemini chunk 流 → OpenAI chunk 的有状态转换器。
/// Gemini 无 [DONE] 标记：finishReason chunk 即终局（finish + usage + Done）。
pub struct GeminiStreamState {
    model: String,
    created: i64,
    tool_index: i64,
    finished: bool,
}

impl GeminiStreamState {
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            created: chrono::Utc::now().timestamp(),
            tool_index: -1,
            finished: false,
        }
    }

    /// 处理一条原生 chunk（data 行 JSON），产出 0..n 条 OpenAI 形状事件。
    pub fn step(
        &mut self,
        item: Result<String, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        let data = match item {
            Ok(data) => data,
            Err(err) => return vec![Err(err)],
        };
        let src: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        if let Some(err) = src.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream_error");
            return vec![Err(UpstreamError::Stream(msg.to_owned()))];
        }

        let mut out = Vec::new();
        let (text, reasoning, tool_calls) = collect_parts(&src);
        if !reasoning.is_empty() {
            let chars = reasoning.chars().count();
            out.push(Ok(self.chunk(
                &json!({"reasoning_content": reasoning}),
                None,
                true,
                chars,
                None,
            )));
        }
        if !text.is_empty() {
            let chars = text.chars().count();
            out.push(Ok(self.chunk(
                &json!({"content": text}),
                None,
                true,
                chars,
                None,
            )));
        }
        for call in &tool_calls {
            self.tool_index += 1;
            let mut call = call.clone();
            if let Some(obj) = call.as_object_mut() {
                obj.insert("index".into(), json!(self.tool_index));
                obj.insert("id".into(), json!(format!("call_{}", self.tool_index)));
            }
            let chars = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .map_or(0, |s| s.chars().count());
            out.push(Ok(self.chunk(
                &json!({"tool_calls": [call]}),
                None,
                true,
                chars,
                None,
            )));
        }

        let finish = src
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str);
        if let Some(finish) = finish
            && !self.finished
        {
            self.finished = true;
            let usage = usage_from_gemini(src.get("usageMetadata"));
            out.push(Ok(self.chunk(
                &json!({}),
                Some(map_finish(Some(finish), self.tool_index >= 0)),
                false,
                0,
                None,
            )));
            out.push(Ok(ChatEvent::Data {
                raw: json!({
                    "id": "gemini",
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [],
                    "usage": usage_json(usage),
                })
                .to_string(),
                event: None,
                has_output: false,
                content_chars: 0,
                usage: Some(usage),
            }));
            out.push(Ok(ChatEvent::Done));
        }
        out
    }

    fn chunk(
        &self,
        delta: &Value,
        finish_reason: Option<&str>,
        has_output: bool,
        content_chars: usize,
        usage: Option<UsageProbe>,
    ) -> ChatEvent {
        let chunk = json!({
            "id": "gemini",
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output,
            content_chars,
            usage,
        }
    }
}

// ---- 组合入口（gateway 用） ----

/// OpenAI 协议入口 + Gemini 上游 的完整一跳（返回 OpenAI 形状）。
pub async fn chat(
    upstream: &GeminiUpstream,
    api_base: &str,
    credential: &str,
    body_gemini: Bytes,
    upstream_model: &str,
    stream: bool,
) -> Result<ChatResponse, UpstreamError> {
    match upstream
        .generate(api_base, credential, upstream_model, body_gemini, stream)
        .await?
    {
        GeminiResponse::Json {
            status,
            upstream_request_id,
            body,
        } => {
            let (body, usage) = response_gemini_to_openai(&body, upstream_model)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            })
        }
        GeminiResponse::Stream(h) => {
            let mut st = GeminiStreamState::new(upstream_model);
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
