//! Anthropic 协议客户端 → OpenAI(兼容) 上游：
//! 请求 Anthropic→OpenAI；响应与事件流 OpenAI→Anthropic（原生事件形状）。
//! usage 口径（与 openai_to_anthropic 互逆）：Anthropic 的 input_tokens 不含缓存，
//! 故 input = prompt - cached，cache_read = cached；计费探针仍用 OpenAI 口径。

use crate::error::UpstreamError;
use crate::types::ChatEvent;
use bytes::Bytes;
use okapi_api::UsageProbe;
use serde_json::{Value, json};

// ---- 请求转换 ----

/// Anthropic messages 请求 → OpenAI chat 请求。
/// 流式时强制 `stream_options.include_usage`（Anthropic 协议出口需要终局 usage）。
pub fn request_anthropic_to_openai(
    body: &Bytes,
    upstream_model: &str,
) -> Result<Bytes, UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };

    let mut out = serde_json::Map::new();
    out.insert("model".into(), json!(upstream_model));

    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = src.get("system") {
        let text = system_to_text(system);
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    for msg in src
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        convert_message(
            role,
            msg.get("content").unwrap_or(&Value::Null),
            &mut messages,
        );
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(v) = src.get("max_tokens") {
        out.insert("max_tokens".into(), v.clone());
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = src.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if let Some(stops) = src.get("stop_sequences") {
        out.insert("stop".into(), stops.clone());
    }
    let stream = src.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if stream {
        out.insert("stream".into(), json!(true));
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }

    convert_tools(src, &mut out);

    serde_json::to_vec(&Value::Object(out))
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

fn system_to_text(system: &Value) -> String {
    match system {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// 单条 Anthropic 消息 → 0..n 条 OpenAI 消息
/// （tool_result 块拆为独立 role=tool 消息；其余块聚合为一条）。
fn convert_message(role: &str, content: &Value, out: &mut Vec<Value>) {
    match content {
        Value::String(s) if !s.is_empty() => {
            out.push(json!({"role": role, "content": s}));
        }
        Value::Array(blocks) => {
            let mut parts: Vec<Value> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => parts.push(json!({
                        "type": "text",
                        "text": block.get("text").and_then(Value::as_str).unwrap_or(""),
                    })),
                    Some("image") => {
                        if let Some(part) = image_to_part(block.get("source")) {
                            parts.push(part);
                        }
                    }
                    Some("tool_use") => tool_calls.push(json!({
                        "id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                        "type": "function",
                        "function": {
                            "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                            "arguments": block
                                .get("input")
                                .map_or_else(String::new, std::string::ToString::to_string),
                        }
                    })),
                    Some("tool_result") => out.push(json!({
                        "role": "tool",
                        "tool_call_id": block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        "content": tool_result_text(block.get("content")),
                    })),
                    _ => {}
                }
            }
            if !parts.is_empty() || !tool_calls.is_empty() {
                let mut msg = serde_json::Map::new();
                msg.insert("role".into(), json!(role));
                // 纯单段文本降级为 string content（兼容面最大）
                let only_text = parts.len() == 1
                    && parts[0].get("type").and_then(Value::as_str) == Some("text");
                msg.insert(
                    "content".into(),
                    if parts.is_empty() {
                        Value::Null
                    } else if only_text {
                        parts[0].get("text").cloned().unwrap_or(Value::Null)
                    } else {
                        Value::Array(parts)
                    },
                );
                if !tool_calls.is_empty() {
                    msg.insert("tool_calls".into(), Value::Array(tool_calls));
                }
                out.push(Value::Object(msg));
            }
        }
        _ => {}
    }
}

fn image_to_part(source: Option<&Value>) -> Option<Value> {
    let source = source?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media = source.get("media_type").and_then(Value::as_str)?;
            let data = source.get("data").and_then(Value::as_str)?;
            Some(json!({"type": "image_url",
                "image_url": {"url": format!("data:{media};base64,{data}")}}))
        }
        Some("url") => {
            let url = source.get("url").and_then(Value::as_str)?;
            Some(json!({"type": "image_url", "image_url": {"url": url}}))
        }
        _ => None,
    }
}

fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn convert_tools(src: &serde_json::Map<String, Value>, out: &mut serde_json::Map<String, Value>) {
    let tools: Vec<Value> = src
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(json!({"type": "function", "function": {
                "name": t.get("name").and_then(Value::as_str)?,
                "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({"type":"object"})),
            }}))
        })
        .collect();
    if tools.is_empty() {
        return;
    }
    out.insert("tools".into(), Value::Array(tools));
    match src
        .get("tool_choice")
        .and_then(|c| c.get("type"))
        .and_then(Value::as_str)
    {
        Some("any") => {
            out.insert("tool_choice".into(), json!("required"));
        }
        Some("tool") => {
            if let Some(name) = src
                .get("tool_choice")
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
            {
                out.insert(
                    "tool_choice".into(),
                    json!({"type": "function", "function": {"name": name}}),
                );
            }
        }
        _ => {} // auto 缺省
    }
}

// ---- 响应转换（非流式） ----

/// OpenAI chat.completion → Anthropic message（含计费探针，OpenAI 口径）。
pub fn response_openai_to_anthropic(
    body: &Bytes,
) -> Result<(Bytes, Option<UsageProbe>), UpstreamError> {
    let src: Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let message = src
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or(Value::Null);

    let mut content: Vec<Value> = Vec::new();
    if let Some(thinking) = message.get("reasoning_content").and_then(Value::as_str)
        && !thinking.is_empty()
    {
        content.push(json!({"type": "thinking", "thinking": thinking, "signature": ""}));
    }
    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push(json!({"type": "text", "text": text}));
    }
    for call in message
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
        content.push(json!({
            "type": "tool_use",
            "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
            "name": f.get("name").and_then(Value::as_str).unwrap_or(""),
            "input": input,
        }));
    }

    let finish = src
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let usage: Option<UsageProbe> = src
        .get("usage")
        .and_then(|u| serde_json::from_value(u.clone()).ok());
    let probe = usage.unwrap_or_default();
    let out = json!({
        "id": src.get("id").and_then(Value::as_str).unwrap_or("msg"),
        "type": "message",
        "role": "assistant",
        "model": src.get("model").and_then(Value::as_str).unwrap_or(""),
        "content": content,
        "stop_reason": map_finish_reason(finish),
        "stop_sequence": Value::Null,
        "usage": anthropic_usage_json(probe),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, Some(probe)))
}

fn map_finish_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    }
}

/// OpenAI 口径探针 → Anthropic usage JSON（input 不含缓存）。
fn anthropic_usage_json(u: UsageProbe) -> Value {
    let cached = u.prompt_tokens_details.cached_tokens.min(u.prompt_tokens);
    json!({
        "input_tokens": u.prompt_tokens - cached,
        "cache_read_input_tokens": cached,
        "cache_creation_input_tokens": 0,
        "output_tokens": u.completion_tokens,
    })
}

// ---- 事件流转换 ----

#[derive(PartialEq, Clone, Copy)]
enum Block {
    None,
    Text,
    Thinking,
    Tool,
}

/// OpenAI chunk 流 → Anthropic 原生事件流的有状态转换器。
/// 事件骨架：message_start → content_block_*（text/thinking/tool_use 换块自动开合）
/// → message_delta（stop_reason + usage）→ message_stop。
pub struct OaiStreamToAnthropic {
    model: String,
    id: String,
    started: bool,
    block: Block,
    block_index: i64,
    finish_reason: Option<String>,
    usage: Option<UsageProbe>,
    finished: bool,
}

impl OaiStreamToAnthropic {
    #[must_use]
    pub fn new(fallback_model: &str) -> Self {
        Self {
            model: fallback_model.to_owned(),
            id: "msg".to_owned(),
            started: false,
            block: Block::None,
            block_index: -1,
            finish_reason: None,
            usage: None,
            finished: false,
        }
    }

    /// 处理一条上游（OpenAI 形状）事件，产出 0..n 条 Anthropic 事件。
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

    #[allow(clippy::too_many_lines)] // 事件骨架的线性映射，拆分反而破坏协议时序可读性
    fn on_chunk(&mut self, chunk: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            if let Some(id) = chunk.get("id").and_then(Value::as_str)
                && !id.is_empty()
            {
                id.clone_into(&mut self.id);
            }
            if let Some(model) = chunk.get("model").and_then(Value::as_str)
                && !model.is_empty()
            {
                model.clone_into(&mut self.model);
            }
            let start = json!({"type": "message_start", "message": {
                "id": self.id, "type": "message", "role": "assistant",
                "model": self.model, "content": [],
                "stop_reason": Value::Null, "stop_sequence": Value::Null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            }});
            out.push(Ok(named("message_start", &start, false, 0, None)));
        }

        let delta = chunk
            .pointer("/choices/0/delta")
            .cloned()
            .unwrap_or_default();

        if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str)
            && !text.is_empty()
        {
            self.ensure_block(Block::Thinking, &mut out);
            let ev = json!({"type": "content_block_delta", "index": self.block_index,
                "delta": {"type": "thinking_delta", "thinking": text}});
            out.push(Ok(named(
                "content_block_delta",
                &ev,
                true,
                text.chars().count(),
                None,
            )));
        }

        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            self.ensure_block(Block::Text, &mut out);
            let ev = json!({"type": "content_block_delta", "index": self.block_index,
                "delta": {"type": "text_delta", "text": text}});
            out.push(Ok(named(
                "content_block_delta",
                &ev,
                true,
                text.chars().count(),
                None,
            )));
        }

        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            // 携带 name/id 视为新工具块；纯 arguments 分片续写当前块
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                self.close_block(&mut out);
                self.block = Block::Tool;
                self.block_index += 1;
                let ev = json!({"type": "content_block_start", "index": self.block_index,
                    "content_block": {"type": "tool_use",
                        "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                        "name": name, "input": {}}});
                out.push(Ok(named("content_block_start", &ev, true, 0, None)));
            }
            if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str)
                && !args.is_empty()
            {
                if self.block != Block::Tool {
                    // 上游未按协议先发 name：兜底开块
                    self.close_block(&mut out);
                    self.block = Block::Tool;
                    self.block_index += 1;
                    let ev = json!({"type": "content_block_start", "index": self.block_index,
                        "content_block": {"type": "tool_use", "id": "", "name": "", "input": {}}});
                    out.push(Ok(named("content_block_start", &ev, true, 0, None)));
                }
                let ev = json!({"type": "content_block_delta", "index": self.block_index,
                    "delta": {"type": "input_json_delta", "partial_json": args}});
                out.push(Ok(named(
                    "content_block_delta",
                    &ev,
                    true,
                    args.chars().count(),
                    None,
                )));
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

    fn ensure_block(&mut self, kind: Block, out: &mut Vec<Result<ChatEvent, UpstreamError>>) {
        if self.block == kind {
            return;
        }
        self.close_block(out);
        self.block = kind;
        self.block_index += 1;
        let block = match kind {
            Block::Thinking => json!({"type": "thinking", "thinking": "", "signature": ""}),
            _ => json!({"type": "text", "text": ""}),
        };
        let ev = json!({"type": "content_block_start", "index": self.block_index,
            "content_block": block});
        out.push(Ok(named("content_block_start", &ev, false, 0, None)));
    }

    fn close_block(&mut self, out: &mut Vec<Result<ChatEvent, UpstreamError>>) {
        if self.block == Block::None {
            return;
        }
        let ev = json!({"type": "content_block_stop", "index": self.block_index});
        out.push(Ok(named("content_block_stop", &ev, false, 0, None)));
        self.block = Block::None;
    }

    /// 终局：补齐块关闭 + message_delta（usage）+ message_stop。
    fn finish(&mut self) -> Vec<Result<ChatEvent, UpstreamError>> {
        if self.finished {
            return vec![Ok(ChatEvent::Done)];
        }
        self.finished = true;
        let mut out = Vec::new();
        if !self.started {
            // 上游一个 chunk 都没给就 DONE：仍给出完整骨架（空产出由 gateway 判空退款）
            let start = json!({"type": "message_start", "message": {
                "id": self.id, "type": "message", "role": "assistant",
                "model": self.model, "content": [],
                "stop_reason": Value::Null, "stop_sequence": Value::Null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            }});
            out.push(Ok(named("message_start", &start, false, 0, None)));
            self.started = true;
        }
        self.close_block(&mut out);
        let probe = self.usage.unwrap_or_default();
        let stop = map_finish_reason(self.finish_reason.as_deref());
        let ev = json!({"type": "message_delta",
            "delta": {"stop_reason": stop, "stop_sequence": Value::Null},
            "usage": anthropic_usage_json(probe)});
        out.push(Ok(named("message_delta", &ev, false, 0, Some(probe))));
        out.push(Ok(named(
            "message_stop",
            &json!({"type": "message_stop"}),
            false,
            0,
            None,
        )));
        out.push(Ok(ChatEvent::Done));
        out
    }
}

fn named(
    event: &str,
    data: &Value,
    has_output: bool,
    content_chars: usize,
    usage: Option<UsageProbe>,
) -> ChatEvent {
    ChatEvent::Data {
        raw: data.to_string(),
        event: Some(event.to_owned()),
        has_output,
        content_chars,
        usage,
    }
}
