use super::super::{chat_chunk, stream_tool_slot};
use super::{content_text, response_completion, responses_usage_json};
use crate::error::UpstreamError;
use crate::types::ChatEvent;
use okapi_api::UsageProbe;
use serde_json::{Value, json};

// ---- 事件流转换 ----

/// Chat chunk 流 → Responses SSE 事件的有状态转换器。
struct StreamTool {
    output_index: usize,
    id: String,
    name: String,
    arguments: String,
}

pub(super) fn reasoning_text(value: &Value) -> Option<&str> {
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
    finish_reason: Option<String>,
    /// assistant message 项（msg_0）的 output_index；文本与拒答是它的两个 content part。
    message_index: Option<usize>,
    next_part: usize,
    text_part: Option<usize>,
    text_buf: String,
    refusal_part: Option<usize>,
    refusal_buf: String,
    reasoning_index: Option<usize>,
    reasoning_buf: String,
    next_index: usize,
    tools: std::collections::BTreeMap<usize, StreamTool>,
    tool_ids: Vec<String>,
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
            finish_reason: None,
            message_index: None,
            next_part: 0,
            text_part: None,
            text_buf: String::new(),
            refusal_part: None,
            refusal_buf: String::new(),
            reasoning_index: None,
            reasoning_buf: String::new(),
            next_index: 0,
            tools: std::collections::BTreeMap::new(),
            tool_ids: Vec::new(),
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
                match chat_chunk(&raw) {
                    Some(Ok(chunk)) => self.on_chunk(&chunk),
                    Some(Err(err)) => vec![Err(err)],
                    None => Vec::new(),
                }
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
            out.push(Ok(self.created()));
        }

        if let Some(reasoning) = chunk
            .pointer("/choices/0/delta")
            .and_then(reasoning_text)
            .filter(|text| !text.is_empty())
        {
            out.extend(self.on_reasoning(reasoning));
        }
        for (field, refusal) in [("content", false), ("refusal", true)] {
            if let Some(text) = chunk
                .pointer("/choices/0/delta")
                .and_then(|delta| delta.get(field))
                .and_then(content_text)
                && !text.is_empty()
            {
                out.extend(self.on_message_delta(&text, refusal));
            }
        }
        out.extend(self.on_tools(chunk));
        out
    }

    /// `response.created`：首个 chunk 时发；上游一个 chunk 都没给就结束时由终局补发，
    /// 不能让客户端先看到 `response.completed`。
    fn created(&mut self) -> ChatEvent {
        self.started = true;
        let payload = json!({"type": "response.created",
            "response": {"id": self.id, "object": "response", "created_at": self.created,
                "status": "in_progress", "model": self.model, "output": []}});
        self.named("response.created", payload, false, 0, None)
    }

    /// 正文或拒答增量：首次出现时开 message 项 / 对应 content part，再发 delta。
    fn on_message_delta(
        &mut self,
        text: &str,
        refusal: bool,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        let mut out = Vec::new();
        let output_index = if let Some(index) = self.message_index {
            index
        } else {
            let index = self.next_index;
            self.next_index += 1;
            self.message_index = Some(index);
            let added = json!({"type": "response.output_item.added", "output_index": index,
                "item": {"type": "message", "id": "msg_0", "status": "in_progress",
                         "role": "assistant", "content": []}});
            out.push(Ok(self.named(
                "response.output_item.added",
                added,
                false,
                0,
                None,
            )));
            index
        };
        let slot = if refusal {
            self.refusal_part
        } else {
            self.text_part
        };
        let content_index = if let Some(part) = slot {
            part
        } else {
            let part = self.next_part;
            self.next_part += 1;
            if refusal {
                self.refusal_part = Some(part);
            } else {
                self.text_part = Some(part);
            }
            let empty = if refusal {
                json!({"type": "refusal", "refusal": ""})
            } else {
                json!({"type": "output_text", "text": "", "annotations": []})
            };
            let added = json!({"type": "response.content_part.added", "item_id": "msg_0",
                "output_index": output_index, "content_index": part, "part": empty});
            out.push(Ok(self.named(
                "response.content_part.added",
                added,
                false,
                0,
                None,
            )));
            part
        };
        let event = if refusal {
            self.refusal_buf.push_str(text);
            "response.refusal.delta"
        } else {
            self.text_buf.push_str(text);
            "response.output_text.delta"
        };
        let delta = json!({"type": event, "item_id": "msg_0",
            "output_index": output_index, "content_index": content_index, "delta": text});
        out.push(Ok(self.named(
            event,
            delta,
            true,
            text.chars().count(),
            None,
        )));
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
                added,
                false,
                0,
                None,
            )));
            let part = json!({"type":"response.reasoning_summary_part.added","item_id":"rs_0",
                    "output_index":index,"summary_index":0,"part":{"type":"summary_text","text":""}});
            out.push(Ok(self.named(
                "response.reasoning_summary_part.added",
                part,
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
            delta,
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
            let Some(index) = stream_tool_slot(delta, &mut self.tool_ids) else {
                return vec![Err(UpstreamError::Stream("tool_count_limit".into()))];
            };
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
                    added,
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
                        payload,
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
        if !self.started {
            out.push(Ok(self.created()));
        }
        let probe = self.usage;
        let mut output = Vec::new();
        if let Some(index) = self.reasoning_index {
            let part = json!({"type":"summary_text","text":self.reasoning_buf});
            let text_done = json!({"type":"response.reasoning_summary_text.done","item_id":"rs_0",
                "output_index":index,"summary_index":0,"text":self.reasoning_buf});
            out.push(Ok(self.named(
                "response.reasoning_summary_text.done",
                text_done,
                false,
                0,
                None,
            )));
            let part_done = json!({"type":"response.reasoning_summary_part.done","item_id":"rs_0",
                "output_index":index,"summary_index":0,"part":part});
            out.push(Ok(self.named(
                "response.reasoning_summary_part.done",
                part_done,
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
                item_done,
                false,
                0,
                None,
            )));
            output.push((index, item));
        }
        if let Some(output_index) = self.message_index {
            let mut parts = Vec::new();
            if let Some(index) = self.text_part {
                let done = json!({"type": "response.output_text.done", "item_id": "msg_0",
                    "output_index": output_index, "content_index": index, "text": self.text_buf});
                out.push(Ok(self.named(
                    "response.output_text.done",
                    done,
                    false,
                    0,
                    None,
                )));
                parts.push((
                    index,
                    json!({"type":"output_text","text":self.text_buf,"annotations":[]}),
                ));
            }
            if let Some(index) = self.refusal_part {
                let done = json!({"type": "response.refusal.done", "item_id": "msg_0",
                    "output_index": output_index, "content_index": index, "refusal": self.refusal_buf});
                out.push(Ok(self.named(
                    "response.refusal.done",
                    done,
                    false,
                    0,
                    None,
                )));
                parts.push((index, json!({"type":"refusal","refusal":self.refusal_buf})));
            }
            parts.sort_by_key(|(index, _)| *index);
            for (index, part) in &parts {
                let part_done = json!({"type":"response.content_part.done","item_id":"msg_0",
                    "output_index":output_index,"content_index":index,"part":part});
                out.push(Ok(self.named(
                    "response.content_part.done",
                    part_done,
                    false,
                    0,
                    None,
                )));
            }
            let content: Vec<Value> = parts.into_iter().map(|(_, part)| part).collect();
            let item = json!({"type":"message","id":"msg_0","status":"completed","role":"assistant","content":content});
            let item_done =
                json!({"type":"response.output_item.done","output_index":output_index,"item":item});
            out.push(Ok(self.named(
                "response.output_item.done",
                item_done,
                false,
                0,
                None,
            )));
            output.push((output_index, item));
        }
        let tools = std::mem::take(&mut self.tools);
        for (index, tool) in tools {
            let item_id = format!("fc_{index}");
            let args_done = json!({"type":"response.function_call_arguments.done","item_id":item_id,"output_index":tool.output_index,"arguments":tool.arguments});
            out.push(Ok(self.named(
                "response.function_call_arguments.done",
                args_done,
                false,
                0,
                None,
            )));
            let item = json!({"type":"function_call","id":item_id,"call_id":tool.id,"name":tool.name,"arguments":tool.arguments,"status":"completed"});
            let done = json!({"type":"response.output_item.done","output_index":tool.output_index,"item":item});
            out.push(Ok(self.named(
                "response.output_item.done",
                done,
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
        out.push(Ok(self.named(terminal, completed, false, 0, probe)));
        out.push(Ok(ChatEvent::Done));
        out
    }

    fn named(
        &mut self,
        event: &str,
        mut data: Value,
        has_output: bool,
        content_chars: usize,
        usage: Option<UsageProbe>,
    ) -> ChatEvent {
        // sequence_number：Responses SSE 规范字段（客户端断点续传参考）。按值收：每个事件都要发，
        // 不必为插这一个字段把整棵树深拷贝一遍
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
