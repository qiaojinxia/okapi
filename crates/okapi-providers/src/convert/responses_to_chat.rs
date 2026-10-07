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
                "content": item.get("output").and_then(Value::as_str).unwrap_or(""),
            }));
        }
        _ => {}
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
    if let Some(reasoning) = reasoning_text(&message).filter(|text| !text.is_empty()) {
        output.push(json!({"type":"reasoning","id":"rs_0","status":"completed",
            "summary":[{"type":"summary_text","text":reasoning}]}));
    }
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

// ---- 事件流转换 ----

/// Chat chunk 流 → Responses SSE 事件的有状态转换器。
struct StreamTool {
    output_index: usize,
    id: String,
    name: String,
    arguments: String,
}

fn reasoning_text(value: &Value) -> Option<&str> {
    value
        .get("reasoning_content")
        .and_then(Value::as_str)
        .or_else(|| value.get("reasoning").and_then(Value::as_str))
}

pub struct ChatStreamToResponses {
    model: String,
    id: String,
    created: i64,
    started: bool,
    text_open: bool,
    finish_reason: Option<String>,
    text_buf: String,
    text_index: usize,
    reasoning_index: Option<usize>,
    reasoning_buf: String,
    next_index: usize,
    tools: std::collections::BTreeMap<usize, StreamTool>,
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
            finish_reason: None,
            text_buf: String::new(),
            text_index: 0,
            reasoning_index: None,
            reasoning_buf: String::new(),
            next_index: 0,
            tools: std::collections::BTreeMap::new(),
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
                    self.usage = Some(u.with_previous(self.usage));
                }
                let chunk: Value = serde_json::from_str(&raw).unwrap_or_default();
                self.on_chunk(&chunk)
            }
        }
    }

    fn on_chunk(&mut self, chunk: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let mut out = Vec::new();
        if let Some(reason) = chunk
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            self.finish_reason = Some(reason.to_owned());
        }
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

        if let Some(reasoning) = chunk
            .pointer("/choices/0/delta")
            .and_then(reasoning_text)
            .filter(|text| !text.is_empty())
        {
            out.extend(self.on_reasoning(reasoning));
        }
        if let Some(text) = chunk
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            && !text.is_empty()
        {
            if !self.text_open {
                self.text_open = true;
                self.text_index = self.next_index;
                self.next_index += 1;
                let added = json!({"type": "response.output_item.added", "output_index": self.text_index,
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
                    "output_index": self.text_index, "content_index": 0,
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
                "output_index": self.text_index, "content_index": 0, "delta": text});
            out.push(Ok(self.named(
                "response.output_text.delta",
                &delta,
                true,
                text.chars().count(),
                None,
            )));
        }
        out.extend(self.on_tools(chunk));
        out
    }

    fn on_reasoning(&mut self, text: &str) -> Vec<Result<ChatEvent, UpstreamError>> {
        if self.reasoning_buf.len().saturating_add(text.len()) > 16 * 1024 * 1024 {
            return vec![Err(UpstreamError::Stream("reasoning_summary_limit".into()))];
        }
        let mut out = Vec::new();
        let index = if let Some(index) = self.reasoning_index {
            index
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.reasoning_index = Some(index);
            let added = json!({"type":"response.output_item.added","output_index":index,
                    "item":{"type":"reasoning","id":"rs_0","status":"in_progress","summary":[]}});
            out.push(Ok(self.named(
                "response.output_item.added",
                &added,
                false,
                0,
                None,
            )));
            let part = json!({"type":"response.reasoning_summary_part.added","item_id":"rs_0",
                    "output_index":index,"summary_index":0,"part":{"type":"summary_text","text":""}});
            out.push(Ok(self.named(
                "response.reasoning_summary_part.added",
                &part,
                false,
                0,
                None,
            )));
            index
        };
        self.reasoning_buf.push_str(text);
        let delta = json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_0",
            "output_index":index,"summary_index":0,"delta":text});
        out.push(Ok(self.named(
            "response.reasoning_summary_text.delta",
            &delta,
            true,
            text.chars().count(),
            None,
        )));
        out
    }

    fn on_tools(&mut self, chunk: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let mut out = Vec::new();
        for delta in chunk
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(index) = delta
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| usize::try_from(i).ok())
            else {
                continue;
            };
            if index >= 128 {
                return vec![Err(UpstreamError::Stream("tool_count_limit".into()))];
            }
            if !self.tools.contains_key(&index) {
                let tool = StreamTool {
                    output_index: self.next_index,
                    id: delta
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    name: String::new(),
                    arguments: String::new(),
                };
                self.next_index += 1;
                let added = json!({"type":"response.output_item.added","output_index":tool.output_index,"item":{"type":"function_call","id":format!("fc_{index}"),"call_id":tool.id,"name":delta.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),"arguments":"","status":"in_progress"}});
                self.tools.insert(index, tool);
                out.push(Ok(self.named(
                    "response.output_item.added",
                    &added,
                    true,
                    0,
                    None,
                )));
            }
            let tool = self.tools.get_mut(&index);
            if let Some(tool) = tool {
                if let Some(id) = delta.get("id").and_then(Value::as_str) {
                    id.clone_into(&mut tool.id);
                }
                if let Some(name) = delta.pointer("/function/name").and_then(Value::as_str) {
                    tool.name.push_str(name);
                }
                if let Some(args) = delta.pointer("/function/arguments").and_then(Value::as_str) {
                    if tool.arguments.len().saturating_add(args.len()) > 16 * 1024 * 1024 {
                        return vec![Err(UpstreamError::Stream("tool_arguments_limit".into()))];
                    }
                    tool.arguments.push_str(args);
                    let payload = json!({"type":"response.function_call_arguments.delta","item_id":format!("fc_{index}"),"output_index":tool.output_index,"delta":args});
                    out.push(Ok(self.named(
                        "response.function_call_arguments.delta",
                        &payload,
                        true,
                        args.chars().count(),
                        None,
                    )));
                }
            }
        }
        out
    }

    #[allow(clippy::too_many_lines)] // Ordered protocol terminal events share one lifecycle.
    fn finish(&mut self) -> Vec<Result<ChatEvent, UpstreamError>> {
        if self.finished {
            return vec![Ok(ChatEvent::Done)];
        }
        self.finished = true;
        let mut out = Vec::new();
        let probe = self.usage;
        if self.text_open {
            let done = json!({"type": "response.output_text.done", "item_id": "msg_0",
                "output_index": self.text_index, "content_index": 0, "text": self.text_buf});
            out.push(Ok(self.named(
                "response.output_text.done",
                &done,
                false,
                0,
                None,
            )));
        }
        let mut output = Vec::new();
        if let Some(index) = self.reasoning_index {
            let part = json!({"type":"summary_text","text":self.reasoning_buf});
            let text_done = json!({"type":"response.reasoning_summary_text.done","item_id":"rs_0",
                "output_index":index,"summary_index":0,"text":self.reasoning_buf});
            out.push(Ok(self.named(
                "response.reasoning_summary_text.done",
                &text_done,
                false,
                0,
                None,
            )));
            let part_done = json!({"type":"response.reasoning_summary_part.done","item_id":"rs_0",
                "output_index":index,"summary_index":0,"part":part});
            out.push(Ok(self.named(
                "response.reasoning_summary_part.done",
                &part_done,
                false,
                0,
                None,
            )));
            let item =
                json!({"type":"reasoning","id":"rs_0","status":"completed","summary":[part]});
            let item_done =
                json!({"type":"response.output_item.done","output_index":index,"item":item});
            out.push(Ok(self.named(
                "response.output_item.done",
                &item_done,
                false,
                0,
                None,
            )));
            output.push((index, item));
        }
        if self.text_open {
            let part = json!({"type":"output_text","text":self.text_buf,"annotations":[]});
            let part_done = json!({"type":"response.content_part.done","item_id":"msg_0","output_index":self.text_index,"content_index":0,"part":part});
            out.push(Ok(self.named(
                "response.content_part.done",
                &part_done,
                false,
                0,
                None,
            )));
            let item = json!({"type":"message","id":"msg_0","status":"completed","role":"assistant","content":[part]});
            let item_done = json!({"type":"response.output_item.done","output_index":self.text_index,"item":item});
            out.push(Ok(self.named(
                "response.output_item.done",
                &item_done,
                false,
                0,
                None,
            )));
            output.push((self.text_index, item));
        }
        let tools = std::mem::take(&mut self.tools);
        for (index, tool) in tools {
            let item_id = format!("fc_{index}");
            let args_done = json!({"type":"response.function_call_arguments.done","item_id":item_id,"output_index":tool.output_index,"arguments":tool.arguments});
            out.push(Ok(self.named(
                "response.function_call_arguments.done",
                &args_done,
                false,
                0,
                None,
            )));
            let item = json!({"type":"function_call","id":item_id,"call_id":tool.id,"name":tool.name,"arguments":tool.arguments,"status":"completed"});
            let done = json!({"type":"response.output_item.done","output_index":tool.output_index,"item":item});
            out.push(Ok(self.named(
                "response.output_item.done",
                &done,
                false,
                0,
                None,
            )));
            output.push((tool.output_index, item));
        }
        output.sort_by_key(|(index, _)| *index);
        let output: Vec<_> = output.into_iter().map(|(_, item)| item).collect();
        let (status, details) = response_completion(self.finish_reason.as_deref());
        let terminal = if status == "incomplete" {
            "response.incomplete"
        } else {
            "response.completed"
        };
        let completed = json!({"type": terminal, "response": {
            "id": self.id, "object": "response", "created_at": self.created,
            "status": status, "incomplete_details": details, "model": self.model,
            "output": output,
            "usage": probe.map(responses_usage_json),
        }});
        out.push(Ok(self.named(terminal, &completed, false, 0, probe)));
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

#[cfg(test)]
mod third_review_tests {
    use super::*;

    #[test]
    fn reasoning_fragments_survive_and_only_delta_characters_are_counted() {
        for with_usage in [false, true] {
            let mut state = ChatStreamToResponses::new("fixture");
            let mut events = Vec::new();
            let usage = serde_json::from_value::<UsageProbe>(json!({"prompt_tokens":7,"completion_tokens":10,"completion_tokens_details":{"reasoning_tokens":3}})).unwrap();
            for (value, probe) in [
                (
                    json!({"choices":[{"delta":{"reasoning_content":"思考"}}]}),
                    None,
                ),
                (json!({"choices":[{"delta":{"reasoning":" done"}}]}), None),
                (json!({"choices":[{"delta":{"content":"OK"}}]}), None),
                (json!({"choices":[]}), with_usage.then_some(usage)),
            ] {
                events.extend(state.step(Ok(ChatEvent::Data {
                    raw: value.to_string(),
                    event: None,
                    has_output: false,
                    content_chars: 0,
                    usage: probe,
                })));
            }
            events.extend(state.step(Ok(ChatEvent::Done)));
            let counted: usize = events
                .iter()
                .filter_map(|event| match event {
                    Ok(ChatEvent::Data { content_chars, .. }) => Some(*content_chars),
                    _ => None,
                })
                .sum();
            assert_eq!(counted, "思考 doneOK".chars().count());
            assert!(events.iter().any(|event| matches!(event,Ok(ChatEvent::Data{has_output:true,event:Some(name),..}) if name=="response.reasoning_summary_text.delta")));
            let completed = events
                .iter()
                .find_map(|event| match event {
                    Ok(ChatEvent::Data {
                        raw,
                        event: Some(name),
                        ..
                    }) if name == "response.completed" => serde_json::from_str::<Value>(raw).ok(),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                completed["response"]["output"][0]["summary"][0]["text"],
                "思考 done"
            );
            assert_eq!(
                completed["response"]["output"][1]["content"][0]["text"],
                "OK"
            );
            if with_usage {
                assert_eq!(completed["response"]["usage"]["output_tokens"], 10);
            } else {
                assert!(completed["response"]["usage"].is_null());
            }
        }
        let (body, _) = response_chat_to_responses(&Bytes::from(
            json!({"choices":[{"message":{"content":"OK","reasoning_content":"think"}}]})
                .to_string(),
        ))
        .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["output"][0]["summary"][0]["text"], "think");
    }
    #[test]
    fn streamed_tool_arguments_survive_fragmentation_and_completion() {
        let mut state = ChatStreamToResponses::new("fixture");
        let data = |value: Value| {
            Ok(ChatEvent::Data {
                raw: value.to_string(),
                event: None,
                has_output: true,
                content_chars: 0,
                usage: None,
            })
        };
        state.step(data(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":"{\"q\":"}}]}}]})));
        let delta=state.step(data(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]}}]})));
        assert!(delta.iter().any(|event|matches!(event,Ok(ChatEvent::Data{event:Some(name),..}) if name=="response.function_call_arguments.delta")));
        let done = state.step(Ok(ChatEvent::Done));
        let completed = done
            .iter()
            .find_map(|event| match event {
                Ok(ChatEvent::Data {
                    raw,
                    event: Some(name),
                    ..
                }) if name == "response.completed" => serde_json::from_str::<Value>(raw).ok(),
                _ => None,
            })
            .unwrap();
        assert_eq!(completed["response"]["output"][0]["name"], "search");
        assert_eq!(completed["response"]["output"][0]["call_id"], "call_1");
        assert_eq!(
            completed["response"]["output"][0]["arguments"],
            r#"{"q":"hi"}"#
        );
    }
}

#[cfg(test)]
mod part_tests {
    use super::*;

    fn convert(input: &Value) -> Result<Value, UpstreamError> {
        let body = Bytes::from(json!({"model": "m", "input": input}).to_string());
        request_responses_to_chat(&body, "up").map(|out| serde_json::from_slice(&out).unwrap())
    }

    /// 内联文件与 refusal 照样带过去；chat 表达不了的引用报错，不能静默丢掉。
    #[test]
    fn files_and_refusals_carry_over_and_references_are_rejected() {
        let out = convert(&json!([
            {"role": "user", "content": [
                {"type": "input_text", "text": "read"},
                {"type": "input_file", "filename": "a.pdf",
                 "file_data": "data:application/pdf;base64,JVBERi0="}
            ]},
            {"role": "assistant", "content": [{"type": "refusal", "refusal": "no"}]}
        ]))
        .unwrap();
        assert_eq!(
            out["messages"][0]["content"][1],
            json!({"type": "file", "file": {
                "filename": "a.pdf", "file_data": "data:application/pdf;base64,JVBERi0="}})
        );
        assert_eq!(out["messages"][1]["content"], "no");
        for part in [
            json!({"type": "input_file", "file_id": "file-1"}),
            json!({"type": "input_image", "file_id": "file-2"}),
        ] {
            let kind = part["type"].as_str().unwrap().to_owned();
            let err = convert(&json!([{"role": "user", "content": [part]}])).unwrap_err();
            assert!(
                matches!(&err, UpstreamError::Build(reason)
                    if *reason == format!("unsupported_content:{kind}")),
                "{err:?}"
            );
        }
    }
}
