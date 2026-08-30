//! thinking-to-content（IMPLEMENTATION §4.4）：客户端不支持 reasoning 输出时，
//! 把 OpenAI 形状事件流/响应里的 `reasoning_content` 转为 `<think>…</think>` 正文。
//! 渠道级开关（channels.settings.thinking_to_content）；仅 OpenAI 入口方言生效
//! （Anthropic 方言原生有 thinking 块，无需转换）。

use crate::error::UpstreamError;
use crate::types::ChatEvent;
use bytes::Bytes;
use serde_json::{Value, json};

const OPEN_TAG: &str = "<think>\n";
const CLOSE_TAG: &str = "\n</think>\n";

/// 流式转换器：reasoning 增量改写为带 `<think>` 标签的 content 增量。
#[derive(Default)]
pub struct ThinkingToContent {
    opened: bool,
    closed: bool,
}

impl ThinkingToContent {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn step(
        &mut self,
        item: Result<ChatEvent, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        let Ok(ChatEvent::Data {
            raw,
            event,
            has_output,
            content_chars,
            usage,
        }) = item
        else {
            return vec![item];
        };
        let Ok(mut chunk) = serde_json::from_str::<Value>(&raw) else {
            return vec![Ok(ChatEvent::Data {
                raw,
                event,
                has_output,
                content_chars,
                usage,
            })];
        };
        let Some(delta) = chunk
            .pointer_mut("/choices/0/delta")
            .and_then(Value::as_object_mut)
        else {
            return vec![Ok(ChatEvent::Data {
                raw,
                event,
                has_output,
                content_chars,
                usage,
            })];
        };

        let reasoning = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let content = delta
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        let mut rewritten: Option<String> = None;
        if !reasoning.is_empty() {
            let mut text = String::new();
            if !self.opened {
                self.opened = true;
                text.push_str(OPEN_TAG);
            }
            text.push_str(&reasoning);
            // 思考与正文同 chunk 到达：闭合后拼正文
            if !content.is_empty() {
                self.closed = true;
                text.push_str(CLOSE_TAG);
                text.push_str(&content);
            }
            rewritten = Some(text);
        } else if !content.is_empty() && self.opened && !self.closed {
            self.closed = true;
            rewritten = Some(format!("{CLOSE_TAG}{content}"));
        }

        let Some(text) = rewritten else {
            return vec![Ok(ChatEvent::Data {
                raw,
                event,
                has_output,
                content_chars,
                usage,
            })];
        };
        let chars = text.chars().count();
        delta.remove("reasoning_content");
        delta.insert("content".into(), json!(text));
        vec![Ok(ChatEvent::Data {
            raw: chunk.to_string(),
            event,
            has_output: true,
            content_chars: chars,
            usage,
        })]
    }
}

/// 非流式：message.reasoning_content → `<think>…</think>` 前缀正文。
#[must_use]
pub fn rewrite_json(body: &Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return body.clone();
    };
    let Some(message) = value
        .pointer_mut("/choices/0/message")
        .and_then(Value::as_object_mut)
    else {
        return body.clone();
    };
    let Some(reasoning) = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return body.clone();
    };
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    message.remove("reasoning_content");
    message.insert(
        "content".into(),
        json!(format!("{OPEN_TAG}{reasoning}{CLOSE_TAG}{content}")),
    );
    serde_json::to_vec(&value).map_or_else(|_| body.clone(), Bytes::from)
}
