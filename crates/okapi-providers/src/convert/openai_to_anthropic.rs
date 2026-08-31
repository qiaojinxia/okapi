//! OpenAI 协议客户端 → Anthropic 上游：
//! 请求 OpenAI→Anthropic；响应与事件流 Anthropic→OpenAI（chunk 形状），
//! 让 gateway 的首字缓冲/泵送/结算对 provider 完全无感。
//!
//! usage 映射基线：Anthropic 的 `input_tokens` 不含缓存读写，OpenAI 的
//! `prompt_tokens` 含缓存 —— 故 prompt = input + cache_read + cache_creation，
//! `cached_tokens` = cache_read（cache_creation 按普通 prompt 计，1.25x 溢价不建模）。

use crate::anthropic::{AnthropicEvent, AnthropicUpstream, MessagesResponse};
use crate::error::UpstreamError;
use crate::openai::{ChatResponse, StreamHandle};
use crate::types::ChatEvent;
use bytes::Bytes;
use futures::StreamExt;
use okapi_api::{CompletionTokensDetails, PromptTokensDetails, UsageProbe};
use serde_json::{Value, json};

// ---- 请求转换 ----

/// OpenAI chat 请求 → Anthropic messages 请求。
/// `default_max_tokens`：OpenAI 侧未显式给上限时的兜底（Anthropic 必填 max_tokens）。
pub fn request_openai_to_anthropic(
    body: &Bytes,
    upstream_model: &str,
    default_max_tokens: u32,
) -> Result<Bytes, UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };

    let mut out = serde_json::Map::new();
    out.insert("model".into(), json!(upstream_model));

    let max_tokens = src
        .get("max_completion_tokens")
        .or_else(|| src.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(default_max_tokens));
    out.insert("max_tokens".into(), json!(max_tokens));

    // messages：system/developer 抽为顶层 system；tool 消息并入 user turn；
    // 连续同角色合并（Anthropic 要求 user/assistant 交替）
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for msg in src
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" | "developer" => {
                let text = content_to_text(msg.get("content").unwrap_or(&Value::Null));
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "tool" => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": msg.get("tool_call_id").and_then(Value::as_str).unwrap_or(""),
                    "content": content_to_text(msg.get("content").unwrap_or(&Value::Null)),
                });
                push_merged(&mut messages, "user", vec![block]);
            }
            "assistant" => {
                let mut blocks = content_to_blocks(msg.get("content").unwrap_or(&Value::Null));
                for call in msg
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let f = call.get("function").cloned().unwrap_or(Value::Null);
                    let input = f
                        .get("arguments")
                        .and_then(Value::as_str)
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or_else(|| json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                        "name": f.get("name").and_then(Value::as_str).unwrap_or(""),
                        "input": input,
                    }));
                }
                if !blocks.is_empty() {
                    push_merged(&mut messages, "assistant", blocks);
                }
            }
            _ => {
                let blocks = content_to_blocks(msg.get("content").unwrap_or(&Value::Null));
                if !blocks.is_empty() {
                    push_merged(&mut messages, "user", blocks);
                }
            }
        }
    }
    if !system_parts.is_empty() {
        out.insert("system".into(), json!(system_parts.join("\n\n")));
    }
    out.insert("messages".into(), Value::Array(messages));

    for key in ["temperature", "top_p", "stream"] {
        if let Some(v) = src.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    match src.get("stop") {
        Some(Value::String(s)) => {
            out.insert("stop_sequences".into(), json!([s]));
        }
        Some(Value::Array(a)) => {
            out.insert("stop_sequences".into(), Value::Array(a.clone()));
        }
        _ => {}
    }

    convert_tools(src, &mut out);

    serde_json::to_vec(&Value::Object(out))
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// tools / tool_choice 映射；tool_choice="none" 时整体不带 tools。
fn convert_tools(src: &serde_json::Map<String, Value>, out: &mut serde_json::Map<String, Value>) {
    let choice = src.get("tool_choice");
    if choice.and_then(Value::as_str) == Some("none") {
        return;
    }
    let tools: Vec<Value> = src
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(json!({
                "name": f.get("name").and_then(Value::as_str)?,
                "description": f.get("description").and_then(Value::as_str).unwrap_or(""),
                "input_schema": f.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
            }))
        })
        .collect();
    if tools.is_empty() {
        return;
    }
    out.insert("tools".into(), Value::Array(tools));
    match choice {
        Some(Value::String(s)) if s == "required" => {
            out.insert("tool_choice".into(), json!({"type": "any"}));
        }
        Some(Value::Object(o)) => {
            if let Some(name) = o
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
            {
                out.insert("tool_choice".into(), json!({"type": "tool", "name": name}));
            }
        }
        _ => {} // auto 缺省
    }
}

/// 连续同角色 turn 合并（内容块级追加）。
fn push_merged(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.extend(blocks);
        return;
    }
    messages.push(json!({"role": role, "content": blocks}));
}

/// OpenAI content（string | parts 数组）→ Anthropic 内容块。
fn content_to_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) if !s.is_empty() => vec![json!({"type": "text", "text": s})],
        Value::Array(parts) => parts.iter().filter_map(part_to_block).collect(),
        _ => Vec::new(),
    }
}

fn part_to_block(part: &Value) -> Option<Value> {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => {
            let text = part.get("text").and_then(Value::as_str)?;
            Some(json!({"type": "text", "text": text}))
        }
        Some("image_url") => {
            let url = part.get("image_url")?.get("url").and_then(Value::as_str)?;
            if let Some(rest) = url.strip_prefix("data:") {
                let (media_type, data) = rest.split_once(";base64,")?;
                Some(json!({"type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": data}}))
            } else {
                Some(json!({"type": "image", "source": {"type": "url", "url": url}}))
            }
        }
        _ => None,
    }
}

fn content_to_text(content: &Value) -> String {
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

// ---- 响应转换（非流式） ----

/// Anthropic message 响应 → OpenAI chat.completion（含 usage 探针）。
pub fn response_anthropic_to_openai(
    body: &Bytes,
) -> Result<(Bytes, Option<UsageProbe>), UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for block in src
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("thinking") => {
                reasoning.push_str(block.get("thinking").and_then(Value::as_str).unwrap_or(""));
            }
            Some("tool_use") => tool_calls.push(json!({
                "id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                "type": "function",
                "function": {
                    "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                    "arguments": block.get("input").map_or_else(String::new, std::string::ToString::to_string),
                }
            })),
            _ => {}
        }
    }

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
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    let stop_reason = src.get("stop_reason").and_then(Value::as_str);
    let usage = usage_from_anthropic(src.get("usage"));
    let out = json!({
        "id": src.get("id").and_then(Value::as_str).unwrap_or("msg"),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": src.get("model").and_then(Value::as_str).unwrap_or(""),
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": map_stop_reason(stop_reason),
        }],
        "usage": usage_json(usage),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, Some(usage)))
}

fn map_stop_reason(stop: Option<&str>) -> &'static str {
    match stop {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        Some("refusal") => "content_filter",
        // end_turn / stop_sequence / 缺省
        _ => "stop",
    }
}

/// Anthropic usage 对象 → OpenAI 口径探针（prompt 含缓存读写；cached = cache_read）。
#[must_use]
pub fn usage_from_anthropic(usage: Option<&Value>) -> UsageProbe {
    let get = |k: &str| {
        usage
            .and_then(|u| u.get(k))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0)
    };
    let input = get("input_tokens");
    let cache_read = get("cache_read_input_tokens");
    let cache_creation = get("cache_creation_input_tokens");
    UsageProbe {
        prompt_tokens: input
            .saturating_add(cache_read)
            .saturating_add(cache_creation),
        completion_tokens: get("output_tokens"),
        prompt_tokens_details: PromptTokensDetails {
            cached_tokens: cache_read,
            cache_write_tokens: cache_creation,
            // Anthropic 无模态细分（图片并入 input_tokens）
            audio_tokens: 0,
            image_tokens: 0,
        },
        completion_tokens_details: CompletionTokensDetails {
            reasoning_tokens: 0,
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
    })
}

// ---- 事件流转换 ----

/// Anthropic SSE 事件 → OpenAI chunk 的有状态转换器。
/// 事件序列：message_start → content_block_* → message_delta（stop_reason+usage）→ message_stop。
pub struct StreamState {
    id: String,
    model: String,
    created: i64,
    input_tokens: u32,
    cache_read: u32,
    cache_creation: u32,
    /// OpenAI tool_calls 数组下标（Anthropic content block index 与其不同构）。
    tool_index: i64,
    /// 当前 Anthropic block index → 是否 tool_use（input_json_delta 归属判定）。
    current_block_is_tool: bool,
}

impl StreamState {
    #[must_use]
    pub fn new(fallback_model: &str) -> Self {
        Self {
            id: "msg".to_owned(),
            model: fallback_model.to_owned(),
            created: chrono::Utc::now().timestamp(),
            input_tokens: 0,
            cache_read: 0,
            cache_creation: 0,
            tool_index: -1,
            current_block_is_tool: false,
        }
    }

    /// 处理一条上游事件，产出 0..n 条 OpenAI 形状事件。
    pub fn step(
        &mut self,
        item: Result<AnthropicEvent, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        let ev = match item {
            Ok(ev) => ev,
            Err(err) => return vec![Err(err)],
        };
        let data: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
        match ev.event.as_str() {
            "message_start" => self.on_message_start(&data),
            "content_block_start" => self.on_block_start(&data),
            "content_block_delta" => self.on_block_delta(&data),
            "message_delta" => self.on_message_delta(&data),
            "message_stop" => vec![Ok(ChatEvent::Done)],
            "error" => {
                let msg = data
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("upstream_error");
                vec![Err(UpstreamError::Stream(msg.to_owned()))]
            }
            // ping / content_block_stop / 未知事件
            _ => Vec::new(),
        }
    }

    fn on_message_start(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        if let Some(m) = data.get("message") {
            if let Some(id) = m.get("id").and_then(Value::as_str) {
                id.clone_into(&mut self.id);
            }
            if let Some(model) = m.get("model").and_then(Value::as_str) {
                model.clone_into(&mut self.model);
            }
            let get = |k: &str| {
                m.get("usage")
                    .and_then(|u| u.get(k))
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(0)
            };
            self.input_tokens = get("input_tokens");
            self.cache_read = get("cache_read_input_tokens");
            self.cache_creation = get("cache_creation_input_tokens");
        }
        // 角色 chunk（content 为空串：不触发首字判定）
        vec![Ok(self.data_event(
            &json!({"role": "assistant", "content": ""}),
            None,
            false,
            0,
            None,
        ))]
    }

    fn on_block_start(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let block = data.get("content_block");
        self.current_block_is_tool =
            block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("tool_use");
        if !self.current_block_is_tool {
            return Vec::new();
        }
        self.tool_index += 1;
        let delta = json!({"tool_calls": [{
            "index": self.tool_index,
            "id": block.and_then(|b| b.get("id")).and_then(Value::as_str).unwrap_or(""),
            "type": "function",
            "function": {
                "name": block.and_then(|b| b.get("name")).and_then(Value::as_str).unwrap_or(""),
                "arguments": "",
            }
        }]});
        vec![Ok(self.data_event(&delta, None, true, 0, None))]
    }

    fn on_block_delta(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(delta) = data.get("delta") else {
            return Vec::new();
        };
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                let chars = text.chars().count();
                vec![Ok(self.data_event(
                    &json!({"content": text}),
                    None,
                    true,
                    chars,
                    None,
                ))]
            }
            Some("thinking_delta") => {
                let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                let chars = text.chars().count();
                vec![Ok(self.data_event(
                    &json!({"reasoning_content": text}),
                    None,
                    true,
                    chars,
                    None,
                ))]
            }
            Some("input_json_delta") => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let chars = partial.chars().count();
                let d = json!({"tool_calls": [{
                    "index": self.tool_index.max(0),
                    "function": {"arguments": partial},
                }]});
                vec![Ok(self.data_event(&d, None, true, chars, None))]
            }
            // signature_delta 等
            _ => Vec::new(),
        }
    }

    fn on_message_delta(&mut self, data: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let stop_reason = data
            .get("delta")
            .and_then(|d| d.get("stop_reason"))
            .and_then(Value::as_str);
        let output = data
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let usage = UsageProbe {
            prompt_tokens: self
                .input_tokens
                .saturating_add(self.cache_read)
                .saturating_add(self.cache_creation),
            completion_tokens: output,
            prompt_tokens_details: PromptTokensDetails {
                cached_tokens: self.cache_read,
                cache_write_tokens: self.cache_creation,
            // Anthropic 无模态细分（图片并入 input_tokens）
                audio_tokens: 0,
                image_tokens: 0,
            },
            completion_tokens_details: CompletionTokensDetails {
                reasoning_tokens: 0,
                audio_tokens: 0,
            },
        };
        vec![
            Ok(self.data_event(
                &json!({}),
                Some(map_stop_reason(stop_reason)),
                false,
                0,
                None,
            )),
            Ok(self.usage_event(usage)),
        ]
    }

    /// 常规 chunk（choices[0].delta）。
    fn data_event(
        &self,
        delta: &Value,
        finish_reason: Option<&str>,
        has_output: bool,
        content_chars: usize,
        usage: Option<UsageProbe>,
    ) -> ChatEvent {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }],
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output,
            content_chars,
            usage,
        }
    }

    /// 终端 usage chunk（choices 为空，OpenAI include_usage 形状）。
    fn usage_event(&self, usage: UsageProbe) -> ChatEvent {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [],
            "usage": usage_json(usage),
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage: Some(usage),
        }
    }
}

// ---- 组合入口（gateway 用） ----

/// OpenAI 协议入口 + Anthropic 上游 的完整一跳：
/// `body_anthropic` 已由 `request_openai_to_anthropic` 转换（forward 循环按候选构造）。
/// 返回 OpenAI 形状的 `ChatResponse`，gateway 泵送与结算零改动。
pub async fn chat(
    upstream: &AnthropicUpstream,
    api_base: &str,
    credential: &str,
    body_anthropic: Bytes,
    upstream_model: &str,
    stream: bool,
) -> Result<ChatResponse, UpstreamError> {
    match upstream
        .messages(api_base, credential, body_anthropic, stream)
        .await?
    {
        MessagesResponse::Json {
            status,
            upstream_request_id,
            body,
        } => {
            let (body, usage) = response_anthropic_to_openai(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            })
        }
        MessagesResponse::Stream(h) => {
            let mut st = StreamState::new(upstream_model);
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
