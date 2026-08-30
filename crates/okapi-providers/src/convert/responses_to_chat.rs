//! OpenAI Responses API 入口 → ChatCompletions 降级（IMPLEMENTATION §4.4，#5209）：
//! 请求 Responses→Chat；响应与 chunk 流 Chat→Responses 事件
//! （response.created → output_item.added → content_part.added →
//! output_text.delta* → *.done → response.completed）。
//! usage 口径：input_tokens = prompt（含缓存）、output_tokens = completion。

use crate::error::UpstreamError;
use crate::types::ChatEvent;
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
    let Some(src) = src.as_object() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };

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
                convert_input_item(item, &mut messages);
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

/// input item → chat 消息：message 项（input_text/output_text/input_image）、
/// function_call / function_call_output 项。
fn convert_input_item(item: &Value, messages: &mut Vec<Value>) {
    match item.get("type").and_then(Value::as_str) {
        // 缺省视为 message 项（Responses 允许省略 type）
        None | Some("message") => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            match item.get("content") {
                Some(Value::String(text)) => {
                    messages.push(json!({"role": role, "content": text}));
                }
                Some(Value::Array(parts)) => {
                    let converted: Vec<Value> = parts
                        .iter()
                        .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                            Some("input_text" | "output_text" | "text") => Some(json!({
                                "type": "text",
                                "text": p.get("text").and_then(Value::as_str).unwrap_or(""),
                            })),
                            Some("input_image") => {
                                let url = p.get("image_url").and_then(Value::as_str)?;
                                Some(json!({"type": "image_url", "image_url": {"url": url}}))
                            }
                            _ => None,
                        })
                        .collect();
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
                "content": item.get("output").and_then(Value::as_str).unwrap_or(""),
            }));
        }
        _ => {}
    }
}

/// Responses 工具形状（扁平 name/parameters）→ chat function 工具。
fn convert_tools(src: &serde_json::Map<String, Value>, out: &mut serde_json::Map<String, Value>) {
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
    if let Some(choice) = src.get("tool_choice") {
        out.insert("tool_choice".into(), choice.clone());
    }
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
    let text = message.get("content").and_then(Value::as_str).unwrap_or("");
    if !text.is_empty() {
        output.push(json!({
            "type": "message", "id": "msg_0", "status": "completed", "role": "assistant",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
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
        .and_then(|u| serde_json::from_value(u.clone()).ok());
    let probe = usage.unwrap_or_default();
    let out = json!({
        "id": format!("resp_{}", src.get("id").and_then(Value::as_str).unwrap_or("0")),
        "object": "response",
        "created_at": src.get("created").and_then(Value::as_i64)
            .unwrap_or_else(|| chrono::Utc::now().timestamp()),
        "status": "completed",
        "model": src.get("model").and_then(Value::as_str).unwrap_or(""),
        "output": output,
        "usage": responses_usage_json(probe),
    });
    let bytes = serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((bytes, Some(probe)))
}

fn responses_usage_json(u: UsageProbe) -> Value {
    json!({
        "input_tokens": u.prompt_tokens,
        "input_tokens_details": {"cached_tokens": u.prompt_tokens_details.cached_tokens},
        "output_tokens": u.completion_tokens,
        "output_tokens_details": {"reasoning_tokens": u.completion_tokens_details.reasoning_tokens},
        "total_tokens": u.prompt_tokens + u.completion_tokens,
    })
}

// ---- 事件流转换 ----

/// Chat chunk 流 → Responses SSE 事件的有状态转换器。
pub struct ChatStreamToResponses {
    model: String,
    id: String,
    created: i64,
    started: bool,
    text_open: bool,
    text_buf: String,
    usage: Option<UsageProbe>,
    finished: bool,
    seq: i64,
}

impl ChatStreamToResponses {
    #[must_use]
    pub fn new(fallback_model: &str) -> Self {
        Self {
            model: fallback_model.to_owned(),
            id: "resp".to_owned(),
            created: chrono::Utc::now().timestamp(),
            started: false,
            text_open: false,
            text_buf: String::new(),
            usage: None,
            finished: false,
            seq: 0,
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
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            if let Some(id) = chunk.get("id").and_then(Value::as_str)
                && !id.is_empty()
            {
                self.id = format!("resp_{id}");
            }
            if let Some(model) = chunk.get("model").and_then(Value::as_str)
                && !model.is_empty()
            {
                model.clone_into(&mut self.model);
            }
            let payload = json!({"type": "response.created",
                "response": {"id": self.id, "object": "response", "created_at": self.created,
                    "status": "in_progress", "model": self.model, "output": []}});
            out.push(Ok(self.named("response.created", &payload, false, 0, None)));
        }

        if let Some(text) = chunk
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            && !text.is_empty()
        {
            if !self.text_open {
                self.text_open = true;
                let added = json!({"type": "response.output_item.added", "output_index": 0,
                    "item": {"type": "message", "id": "msg_0", "status": "in_progress",
                             "role": "assistant", "content": []}});
                out.push(Ok(self.named(
                    "response.output_item.added",
                    &added,
                    false,
                    0,
                    None,
                )));
                let part = json!({"type": "response.content_part.added", "item_id": "msg_0",
                    "output_index": 0, "content_index": 0,
                    "part": {"type": "output_text", "text": "", "annotations": []}});
                out.push(Ok(self.named(
                    "response.content_part.added",
                    &part,
                    false,
                    0,
                    None,
                )));
            }
            self.text_buf.push_str(text);
            let delta = json!({"type": "response.output_text.delta", "item_id": "msg_0",
                "output_index": 0, "content_index": 0, "delta": text});
            out.push(Ok(self.named(
                "response.output_text.delta",
                &delta,
                true,
                text.chars().count(),
                None,
            )));
        }
        out
    }

    fn finish(&mut self) -> Vec<Result<ChatEvent, UpstreamError>> {
        if self.finished {
            return vec![Ok(ChatEvent::Done)];
        }
        self.finished = true;
        let mut out = Vec::new();
        let probe = self.usage.unwrap_or_default();
        if self.text_open {
            let done = json!({"type": "response.output_text.done", "item_id": "msg_0",
                "output_index": 0, "content_index": 0, "text": self.text_buf});
            out.push(Ok(self.named(
                "response.output_text.done",
                &done,
                false,
                0,
                None,
            )));
        }
        let completed = json!({"type": "response.completed", "response": {
            "id": self.id, "object": "response", "created_at": self.created,
            "status": "completed", "model": self.model,
            "output": if self.text_buf.is_empty() { json!([]) } else {
                json!([{"type": "message", "id": "msg_0", "status": "completed",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": self.text_buf, "annotations": []}]}])
            },
            "usage": responses_usage_json(probe),
        }});
        out.push(Ok(self.named(
            "response.completed",
            &completed,
            false,
            0,
            Some(probe),
        )));
        out.push(Ok(ChatEvent::Done));
        out
    }

    fn named(
        &mut self,
        event: &str,
        data: &Value,
        has_output: bool,
        content_chars: usize,
        usage: Option<UsageProbe>,
    ) -> ChatEvent {
        // sequence_number：Responses SSE 规范字段（客户端断点续传参考）
        let mut data = data.clone();
        if let Some(obj) = data.as_object_mut() {
            obj.insert("sequence_number".into(), json!(self.seq));
        }
        self.seq += 1;
        ChatEvent::Data {
            raw: data.to_string(),
            event: Some(event.to_owned()),
            has_output,
            content_chars,
            usage,
        }
    }
}
