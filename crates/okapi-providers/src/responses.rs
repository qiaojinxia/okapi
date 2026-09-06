//! OpenAI Responses API 原生方向（`/v1/responses` 同方言直转，IMPLEMENTATION §4.4）。
//!
//! 与 `convert/responses_to_chat.rs` 的降级链互补：上游本身就说 Responses 方言
//! （openai 原生渠道，或显式声明 `settings.responses_native` 的兼容渠道）时，
//! 请求体**原样透传**——`previous_response_id` / `store` / `include` / 内置工具
//! （web_search、file_search、computer_use、mcp）/ reasoning items 全部保住；
//! 降级链会把这些静默丢掉，Codex CLI 这类客户端表现为"续聊断链、推理项消失"。
//!
//! 本模块只负责传输、事件切分与 usage 解析：事件原文透出（保留 event 名），
//! 提取首字判定 / 字符数 / usage 供 gateway 泵送与结算。

use crate::error::UpstreamError;
use crate::openai::{ChatResponse, OpenAiUpstream, StreamHandle};
use crate::types::ChatEvent;
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use okapi_api::{CompletionTokensDetails, PromptTokensDetails, UsageProbe};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

/// 非流式请求总超时（与 chat 一致；流式由 gateway 首字窗口控制）。
const NON_STREAM_TIMEOUT: Duration = Duration::from_mins(2);

/// Responses 响应对象里的 usage 形状（官方 `ResponseUsage`）。
#[derive(Debug, Default, Clone, Copy, Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    input_tokens_details: InputDetails,
    #[serde(default)]
    output_tokens_details: OutputDetails,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
struct InputDetails {
    #[serde(default)]
    cached_tokens: u32,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
struct OutputDetails {
    #[serde(default)]
    reasoning_tokens: u32,
}

impl From<ResponsesUsage> for UsageProbe {
    fn from(u: ResponsesUsage) -> Self {
        Self {
            // 口径与降级链一致：input_tokens 含缓存命中；output_tokens 含 reasoning
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            prompt_tokens_details: PromptTokensDetails {
                cached_tokens: u.input_tokens_details.cached_tokens,
                cache_write_tokens: 0,
                audio_tokens: 0,
                image_tokens: 0,
            },
            completion_tokens_details: CompletionTokensDetails {
                reasoning_tokens: u.output_tokens_details.reasoning_tokens,
                audio_tokens: 0,
            },
        }
    }
}

/// 从 Responses `usage` 对象解析计费用量（对象缺失或形状不符 → None，交结算兜底估算）。
#[must_use]
pub fn usage_from_responses(usage: Option<&Value>) -> Option<UsageProbe> {
    let raw = usage?;
    if !raw.is_object() {
        return None;
    }
    serde_json::from_value::<ResponsesUsage>(raw.clone())
        .ok()
        .map(UsageProbe::from)
}

/// 终态事件：`response.completed` / `.incomplete` / `.failed` 之后流即结束，
/// 这三种都携带（可能为部分产出的）usage。
fn is_terminal(kind: &str) -> bool {
    matches!(
        kind,
        "response.completed" | "response.incomplete" | "response.failed"
    )
}

/// 一条 Responses SSE 事件 → 透传事件（终态后追加流终止标记）。
///
/// 首字判定：任何带 `delta` 文本的增量事件（output_text / refusal /
/// function_call_arguments / reasoning_summary_text / audio_transcript …）
/// 与完成的输出项（`response.output_item.done`）都算实际产出——工具调用与
/// 纯推理摘要也是花了钱的产出，不能因为没有正文就判成空回复退款。
#[must_use]
pub fn parse_event(event_name: &str, data: &str) -> Vec<ChatEvent> {
    if data.trim() == "[DONE]" {
        // 官方不发 [DONE]；兼容上游若发，当作流结束
        return vec![ChatEvent::Done];
    }
    let parsed: Value = serde_json::from_str(data).unwrap_or_default();
    let kind = parsed
        .get("type")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .unwrap_or(event_name)
        .to_owned();
    let is_delta = kind.strip_suffix(".delta").is_some();
    let (has_output, content_chars) = match parsed.get("delta").and_then(Value::as_str) {
        Some(delta) if is_delta => (true, delta.chars().count()),
        _ => (kind == "response.output_item.done", 0),
    };
    let usage = if is_terminal(&kind) {
        usage_from_responses(parsed.pointer("/response/usage"))
    } else {
        None
    };
    let event_name = if kind.is_empty() {
        None
    } else {
        Some(kind.clone())
    };
    let passthrough = ChatEvent::Data {
        raw: data.to_owned(),
        event: event_name,
        has_output,
        content_chars,
        usage,
    };
    if is_terminal(&kind) {
        vec![passthrough, ChatEvent::Done]
    } else {
        vec![passthrough]
    }
}

impl OpenAiUpstream {
    /// 转发 /v1/responses（同方言直转）。`body` 已完成模型名映射与 reasoning 注入，
    /// 其余字段原样透传；usage 由 Responses 协议保证随 `response.completed` 返回，
    /// 无需像 chat 那样补 `stream_options`。
    pub async fn responses(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
        stream: bool,
        outbound: &crate::http::Outbound,
    ) -> Result<ChatResponse, UpstreamError> {
        let url = format!("{}/responses", api_base.trim_end_matches('/'));
        let mut req = self
            .http
            .post(outbound, url)?
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec());
        if !stream {
            req = req.timeout(NON_STREAM_TIMEOUT);
        }

        let resp = req.send().await.map_err(|e| crate::openai::classify(&e))?;
        let status = resp.status().as_u16();
        let upstream_request_id = resp
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        if !(200..300).contains(&status) {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            let body = resp.bytes().await.unwrap_or_default();
            return Err(UpstreamError::Status {
                status,
                body,
                retry_after_secs,
            });
        }

        if stream {
            let events = resp
                .bytes_stream()
                .eventsource()
                .flat_map(|item| match item {
                    Ok(event) => futures::stream::iter(
                        parse_event(&event.event, &event.data)
                            .into_iter()
                            .map(Ok)
                            .collect::<Vec<_>>(),
                    ),
                    Err(e) => {
                        futures::stream::iter(vec![Err(UpstreamError::Stream(e.to_string()))])
                    }
                });
            Ok(ChatResponse::Stream(StreamHandle {
                upstream_request_id,
                events: Box::pin(events),
            }))
        } else {
            let body = resp
                .bytes()
                .await
                .map_err(|e| crate::openai::classify(&e))?;
            let usage = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| usage_from_responses(v.get("usage")));
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_maps_official_shape() {
        let v = serde_json::json!({
            "input_tokens": 120, "output_tokens": 45,
            "input_tokens_details": {"cached_tokens": 100},
            "output_tokens_details": {"reasoning_tokens": 30},
            "total_tokens": 165
        });
        let u = usage_from_responses(Some(&v)).unwrap();
        assert_eq!(u.prompt_tokens, 120);
        assert_eq!(u.completion_tokens, 45);
        assert_eq!(u.prompt_tokens_details.cached_tokens, 100);
        assert_eq!(u.completion_tokens_details.reasoning_tokens, 30);
        assert!(usage_from_responses(None).is_none());
        assert!(usage_from_responses(Some(&Value::Null)).is_none());
    }

    #[test]
    fn delta_events_count_as_output() {
        let ev = parse_event(
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","delta":"你好 hi"}"#,
        );
        assert_eq!(ev.len(), 1);
        let ChatEvent::Data {
            has_output,
            content_chars,
            event,
            usage,
            ..
        } = &ev[0]
        else {
            panic!("expected data");
        };
        assert!(*has_output);
        assert_eq!(*content_chars, 5);
        assert_eq!(event.as_deref(), Some("response.output_text.delta"));
        assert!(usage.is_none());

        let ev = parse_event(
            "response.function_call_arguments.delta",
            r#"{"type":"response.function_call_arguments.delta","delta":"{\"a\":"}"#,
        );
        assert!(matches!(
            ev[0],
            ChatEvent::Data {
                has_output: true,
                ..
            }
        ));
    }

    #[test]
    fn lifecycle_events_are_not_output() {
        for (name, data) in [
            (
                "response.created",
                r#"{"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}"#,
            ),
            (
                "response.output_item.added",
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning"}}"#,
            ),
        ] {
            let ev = parse_event(name, data);
            assert_eq!(ev.len(), 1);
            assert!(matches!(
                ev[0],
                ChatEvent::Data {
                    has_output: false,
                    ..
                }
            ));
        }
        let ev = parse_event(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","item":{"type":"function_call"}}"#,
        );
        assert!(matches!(
            ev[0],
            ChatEvent::Data {
                has_output: true,
                ..
            }
        ));
    }

    #[test]
    fn completed_carries_usage_and_terminates() {
        let ev = parse_event(
            "response.completed",
            r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed",
                "usage":{"input_tokens":10,"output_tokens":4,
                         "input_tokens_details":{"cached_tokens":0},
                         "output_tokens_details":{"reasoning_tokens":2}}}}"#,
        );
        assert_eq!(ev.len(), 2);
        let ChatEvent::Data { usage, .. } = &ev[0] else {
            panic!("expected data");
        };
        let u = usage.unwrap();
        assert_eq!((u.prompt_tokens, u.completion_tokens), (10, 4));
        assert_eq!(u.completion_tokens_details.reasoning_tokens, 2);
        assert!(matches!(ev[1], ChatEvent::Done));
    }

    #[test]
    fn event_name_falls_back_to_sse_event_line() {
        let ev = parse_event("response.in_progress", r#"{"response":{}}"#);
        assert!(matches!(
            &ev[0],
            ChatEvent::Data { event: Some(name), .. } if name == "response.in_progress"
        ));
        assert!(matches!(parse_event("", "[DONE]")[0], ChatEvent::Done));
    }
}
