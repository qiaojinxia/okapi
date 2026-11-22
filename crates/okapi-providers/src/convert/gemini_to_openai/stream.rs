use super::super::{chat_chunk, stream_tool_slot};
use super::{function_call_part, gemini_usage_json, map_finish, response_openai_to_gemini};
use crate::error::UpstreamError;
use crate::openai::{ChatResponse, StreamHandle};
use crate::types::ChatEvent;
use futures::StreamExt;
use okapi_api::UsageProbe;
use serde_json::{Value, json};

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
    tool_ids: Vec<String>,
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
            tool_ids: Vec::new(),
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
        // 只读：借用即可，不必把整个 delta（含 tool_calls 参数）深拷贝一遍
        let empty = Value::Null;
        let delta = chunk.pointer("/choices/0/delta").unwrap_or(&empty);

        if let Some(text) = super::super::chat_reasoning_text(delta) {
            out.push(Ok(self.chunk(
                &[json!({"text": text, "thought": true})],
                None,
                None,
                text.chars().count(),
            )));
        }
        for text in ["content", "refusal"]
            .into_iter()
            .filter_map(|field| delta.get(field).and_then(Value::as_str))
            .filter(|text| !text.is_empty())
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
            let Some(index) = stream_tool_slot(call, &mut self.tool_ids) else {
                return vec![Err(UpstreamError::Stream("tool_index_invalid".into()))];
            };
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
        let probe = self.usage;
        let finish = map_finish(self.finish_reason.as_deref());
        let chars: usize = calls.iter().map(|c| c.arguments.chars().count()).sum();
        let mut last = self.chunk(&parts, Some(finish), probe, chars);
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
            usage,
        } => {
            let (body, parsed_usage) = response_openai_to_gemini(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage: usage.or(parsed_usage),
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
