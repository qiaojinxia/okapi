use super::{
    append_message_content, append_reasoning_from_item, chat_usage_json,
    finish_reason_from_response, response_responses_to_chat,
};
use crate::error::UpstreamError;
use crate::openai::{ChatResponse, StreamHandle};
use crate::responses::usage_from_responses;
use crate::types::ChatEvent;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

// ---- 流式响应转换 ----

/// Responses SSE 事件流 → OpenAI Chat Completions chunk 流。
pub struct ResponsesStreamToChat {
    id: String,
    created: i64,
    model: String,
    sent_role: bool,
    has_tool_calls: bool,
    finished: bool,
    tool_slots: HashMap<String, usize>,
    next_tool_slot: usize,
    streamed_tool_args: HashSet<usize>,
    streamed_text_items: HashSet<String>,
    streamed_refusal_items: HashSet<String>,
    streamed_reasoning_items: HashSet<String>,
}

impl ResponsesStreamToChat {
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            id: "chatcmpl-resp".to_owned(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_owned(),
            sent_role: false,
            has_tool_calls: false,
            finished: false,
            tool_slots: HashMap::new(),
            next_tool_slot: 0,
            streamed_tool_args: HashSet::new(),
            streamed_text_items: HashSet::new(),
            streamed_refusal_items: HashSet::new(),
            streamed_reasoning_items: HashSet::new(),
        }
    }

    pub fn step(
        &mut self,
        item: Result<ChatEvent, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        match item {
            Err(err) => vec![Err(err)],
            Ok(ChatEvent::Done) => {
                if self.finished {
                    return Vec::new();
                }
                self.finished = true;
                let finish = if self.has_tool_calls {
                    "tool_calls"
                } else {
                    "stop"
                };
                vec![Ok(self.finish_chunk(finish)), Ok(ChatEvent::Done)]
            }
            Ok(ChatEvent::Data { raw, event, .. }) => self.handle_event(&raw, event.as_deref()),
        }
    }

    fn handle_event(
        &mut self,
        raw: &str,
        event_name: Option<&str>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        if raw.trim().is_empty() {
            return Vec::new();
        }
        let Ok(ev) = serde_json::from_str::<Value>(raw) else {
            return vec![Err(UpstreamError::Stream("responses_event_json".into()))];
        };
        let kind = ev
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or(event_name)
            .unwrap_or("");

        match kind {
            "response.created" | "response.in_progress" => {
                if let Some(resp) = ev.get("response") {
                    self.capture_meta(resp);
                }
                Vec::new()
            }
            "response.output_text.delta" => {
                let Some(delta) = ev
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Vec::new();
                };
                if let Some(id) = ev.get("item_id").and_then(Value::as_str) {
                    self.streamed_text_items.insert(id.to_owned());
                }
                vec![Ok(self.delta_chunk(
                    json!({"content": delta}),
                    delta.chars().count(),
                ))]
            }
            "response.refusal.delta" => {
                let Some(delta) = ev
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Vec::new();
                };
                if let Some(id) = ev.get("item_id").and_then(Value::as_str) {
                    self.streamed_refusal_items.insert(id.to_owned());
                }
                vec![Ok(self.delta_chunk(
                    json!({"refusal": delta}),
                    delta.chars().count(),
                ))]
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let Some(delta) = ev
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Vec::new();
                };
                if let Some(id) = ev.get("item_id").and_then(Value::as_str) {
                    self.streamed_reasoning_items.insert(id.to_owned());
                }
                vec![Ok(self.delta_chunk(
                    json!({"reasoning_content": delta}),
                    delta.chars().count(),
                ))]
            }
            "response.output_item.added" => self.handle_item_added(&ev),
            "response.function_call_arguments.delta" => self.handle_tool_args_delta(&ev),
            "response.output_item.done" => self.handle_item_done(&ev),
            "response.completed" | "response.incomplete" => self.handle_terminal(&ev),
            "response.failed" => {
                let msg = ev
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("responses_failed");
                self.finished = true;
                vec![Err(UpstreamError::Stream(msg.to_owned()))]
            }
            _ => Vec::new(),
        }
    }

    fn capture_meta(&mut self, resp: &Value) {
        if let Some(id) = resp
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            id.clone_into(&mut self.id);
        }
        if let Some(created) = resp.get("created_at").and_then(Value::as_i64) {
            self.created = created;
        }
        if let Some(model) = resp
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            model.clone_into(&mut self.model);
        }
    }

    fn slot_key(ev: &Value, item: Option<&Value>) -> String {
        if let Some(id) = item
            .and_then(|i| i.get("id"))
            .or_else(|| ev.get("item_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return id.to_owned();
        }
        ev.get("output_index")
            .and_then(Value::as_u64)
            .map_or_else(|| "0".to_owned(), |n| format!("idx:{n}"))
    }

    fn alloc_tool_slot(&mut self, key: String) -> usize {
        if let Some(&slot) = self.tool_slots.get(&key) {
            return slot;
        }
        let slot = self.next_tool_slot;
        self.next_tool_slot += 1;
        self.tool_slots.insert(key, slot);
        slot
    }

    fn handle_item_added(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(item) = ev.get("item") else {
            return Vec::new();
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Vec::new();
        }
        let key = Self::slot_key(ev, Some(item));
        let slot = self.alloc_tool_slot(key);
        self.has_tool_calls = true;
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let args = item.get("arguments").and_then(Value::as_str).unwrap_or("");
        if !args.is_empty() {
            self.streamed_tool_args.insert(slot);
        }
        vec![Ok(self.delta_chunk(
            json!({
                "tool_calls": [{
                    "index": slot,
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args,
                    }
                }]
            }),
            args.chars().count(),
        ))]
    }

    fn handle_tool_args_delta(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(delta) = ev
            .get("delta")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Vec::new();
        };
        let key = Self::slot_key(ev, None);
        let slot = self.alloc_tool_slot(key);
        self.has_tool_calls = true;
        self.streamed_tool_args.insert(slot);
        vec![Ok(self.delta_chunk(
            json!({
                "tool_calls": [{
                    "index": slot,
                    "function": {
                        "arguments": delta,
                    }
                }]
            }),
            delta.chars().count(),
        ))]
    }

    fn handle_item_done(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Some(item) = ev.get("item") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let key = Self::slot_key(ev, Some(item));
                let existed = self.tool_slots.contains_key(&key);
                let slot = self.alloc_tool_slot(key);
                self.has_tool_calls = true;
                let args = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                if !existed {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    let args_out = if args.is_empty() { "{}" } else { args };
                    self.streamed_tool_args.insert(slot);
                    out.push(Ok(self.delta_chunk(
                        json!({
                            "tool_calls": [{
                                "index": slot,
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": args_out,
                                }
                            }]
                        }),
                        args_out.chars().count(),
                    )));
                } else if !self.streamed_tool_args.contains(&slot) && !args.is_empty() {
                    self.streamed_tool_args.insert(slot);
                    out.push(Ok(self.delta_chunk(
                        json!({
                            "tool_calls": [{
                                "index": slot,
                                "function": {
                                    "arguments": args,
                                }
                            }]
                        }),
                        args.chars().count(),
                    )));
                }
            }
            Some("message") => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                let mut text = String::new();
                let mut refusal = String::new();
                append_message_content(item, &mut text, &mut refusal);
                if !text.is_empty() && !self.streamed_text_items.contains(id) {
                    out.push(Ok(
                        self.delta_chunk(json!({"content": text}), text.chars().count())
                    ));
                }
                if !refusal.is_empty() && !self.streamed_refusal_items.contains(id) {
                    out.push(Ok(self.delta_chunk(
                        json!({"refusal": refusal}),
                        refusal.chars().count(),
                    )));
                }
            }
            Some("reasoning") => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                if !self.streamed_reasoning_items.contains(id) {
                    let mut summary = String::new();
                    append_reasoning_from_item(item, &mut summary);
                    if !summary.is_empty() {
                        out.push(Ok(self.delta_chunk(
                            json!({"reasoning_content": summary}),
                            summary.chars().count(),
                        )));
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn handle_terminal(&mut self, ev: &Value) -> Vec<Result<ChatEvent, UpstreamError>> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let resp = ev.get("response").unwrap_or(&Value::Null);
        self.capture_meta(resp);
        let finish = finish_reason_from_response(resp, self.has_tool_calls);
        let probe = usage_from_responses(resp.get("usage"));
        let usage_json = chat_usage_json(resp.get("usage"), probe);

        let mut out = vec![Ok(self.finish_chunk(finish))];
        if probe.is_some() || usage_json.is_some() {
            let chunk = json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [],
                "usage": usage_json.unwrap_or(Value::Null),
            });
            out.push(Ok(ChatEvent::Data {
                raw: chunk.to_string(),
                event: None,
                has_output: false,
                content_chars: 0,
                usage: probe,
            }));
        }
        out.push(Ok(ChatEvent::Done));
        out
    }

    fn delta_chunk(&mut self, mut delta: Value, content_chars: usize) -> ChatEvent {
        if !self.sent_role {
            self.sent_role = true;
            if let Some(obj) = delta.as_object_mut() {
                obj.insert("role".into(), json!("assistant"));
            }
        }
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": Value::Null,
            }],
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output: true,
            content_chars,
            usage: None,
        }
    }

    fn finish_chunk(&self, finish_reason: &str) -> ChatEvent {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish_reason,
            }],
        });
        ChatEvent::Data {
            raw: chunk.to_string(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage: None,
        }
    }
}

/// 将 Responses 上游响应（JSON 或 SSE 流）包装回 OpenAI Chat Completions 形状。
pub fn wrap_responses_as_chat(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => {
            let (body, parsed_usage) = response_responses_to_chat(&body, upstream_model)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage: usage.or(parsed_usage),
            })
        }
        ChatResponse::Stream(h) => {
            let mut st = ResponsesStreamToChat::new(upstream_model);
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
