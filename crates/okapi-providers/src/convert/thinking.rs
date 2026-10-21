//! thinking-to-content（IMPLEMENTATION §4.4）：客户端不支持 reasoning 输出时，
//! 把 OpenAI 形状事件流/响应里的 `reasoning_content` 转为 `<think>…</think>` 正文。
//! 渠道级开关（channels.settings.thinking_to_content）；仅 OpenAI 入口方言生效
//! （Anthropic 方言原生有 thinking 块，无需转换）。

use crate::error::UpstreamError;
use crate::types::ChatEvent;
use bytes::Bytes;
use serde_json::{Map, Value, json};

const OPEN_TAG: &str = "<think>\n";
const CLOSE_TAG: &str = "\n</think>\n";

/// 把 `lead` 放到 `content` 最前面，原有内容一个字不丢：字符串 / 缺省 / null 拼成字符串；
/// 分段数组（多模态输出的合法形状）在头部插一个 `{"type":"text"}` 片段，原有片段原样保留；
/// 空数组没有东西可保，退成字符串。别的形状认不得，返回 false 且不改动，
/// 调用方原样放行——宁可客户端多看到一个 `reasoning_content`，也不静默吞掉上游内容。
fn prepend_text(holder: &mut Map<String, Value>, lead: &str) -> bool {
    match holder.get_mut("content") {
        Some(Value::String(text)) => text.insert_str(0, lead),
        Some(Value::Array(parts)) if !parts.is_empty() => {
            parts.insert(0, json!({"type": "text", "text": lead}));
        }
        None | Some(Value::Null | Value::Array(_)) => {
            holder.insert("content".into(), json!(lead));
        }
        Some(_) => return false,
    }
    true
}

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
        // 正文既可能是字符串，也可能是分段数组；两者都算"有正文"
        let has_content = match delta.get("content") {
            Some(Value::String(text)) => !text.is_empty(),
            Some(Value::Array(parts)) => !parts.is_empty(),
            _ => false,
        };

        let mut lead = String::new();
        let (mut opened, mut closed) = (self.opened, self.closed);
        if !reasoning.is_empty() {
            if !opened {
                opened = true;
                lead.push_str(OPEN_TAG);
            }
            lead.push_str(&reasoning);
            // 思考与正文同 chunk 到达：闭合后接正文
            if has_content {
                closed = true;
                lead.push_str(CLOSE_TAG);
            }
        } else if has_content && opened && !closed {
            closed = true;
            lead.push_str(CLOSE_TAG);
        }

        if lead.is_empty() || !prepend_text(delta, &lead) {
            return vec![Ok(ChatEvent::Data {
                raw,
                event,
                has_output,
                content_chars,
                usage,
            })];
        }
        (self.opened, self.closed) = (opened, closed);
        delta.remove("reasoning_content");
        // 改写后的 chunk 按统一口径重数（数组正文、拒答、工具参数一并算入）
        let (_, chars) = okapi_api::chunk_output_lenient(&chunk);
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
    if !prepend_text(message, &format!("{OPEN_TAG}{reasoning}{CLOSE_TAG}")) {
        return body.clone();
    }
    message.remove("reasoning_content");
    serde_json::to_vec(&value).map_or_else(|_| body.clone(), Bytes::from)
}
