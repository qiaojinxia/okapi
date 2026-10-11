//! Gemini 原生上游客户端（generateContent / streamGenerateContent?alt=sse）。
//!
//! 只负责传输与事件切分：流式返回原生 GenerateContentResponse chunk 原文，
//! 协议转换在 convert::openai_to_gemini 进行（禁统一 IR）。

use crate::error::UpstreamError;
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use std::pin::Pin;
use std::time::Duration;

const NON_STREAM_TIMEOUT: Duration = Duration::from_mins(2);

pub struct GeminiStream {
    pub upstream_request_id: Option<String>,
    /// 原生 chunk（data 行 JSON 原文）。
    pub events: Pin<Box<dyn Stream<Item = Result<String, UpstreamError>> + Send>>,
}

pub enum GeminiResponse {
    Stream(GeminiStream),
    Json {
        status: u16,
        upstream_request_id: Option<String>,
        body: Bytes,
    },
}

#[derive(Clone)]
pub struct GeminiUpstream {
    http: crate::http::HttpPool,
}

impl GeminiUpstream {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            http: crate::http::HttpPool::new()?,
        })
    }

    /// 转发 generateContent。`body` 已是 Gemini 协议 JSON；模型名走 URL 路径。
    pub async fn generate(
        &self,
        api_base: &str,
        credential: &str,
        model: &str,
        body: Bytes,
        stream: bool,
        outbound: &crate::http::Outbound,
    ) -> Result<GeminiResponse, UpstreamError> {
        let base = api_base.trim_end_matches('/');
        let url = if stream {
            format!("{base}/models/{model}:streamGenerateContent?alt=sse")
        } else {
            format!("{base}/models/{model}:generateContent")
        };
        send_generate_at(
            &self.http,
            url,
            ("x-goog-api-key", credential),
            body,
            stream,
            outbound,
        )
        .await
    }
}

/// 向任意 URL 发一次 Gemini generateContent 形状的请求（鉴权头由调用方给）：直连官方走
/// `x-goog-api-key`，Vertex 走 Bearer（IMPLEMENTATION §11.35）。流式要求调用方 URL 已带
/// `alt=sse`（Vertex 与 AI Studio 同此约定）。
pub async fn send_generate_at(
    http: &crate::http::HttpPool,
    url: String,
    auth_header: (&str, &str),
    body: Bytes,
    stream: bool,
    outbound: &crate::http::Outbound,
) -> Result<GeminiResponse, UpstreamError> {
    let mut req = http
        .post(outbound, url)?
        .header(auth_header.0, auth_header.1)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.clone());
    if !stream {
        req = req.timeout(NON_STREAM_TIMEOUT);
    }

    let resp = req.send().await.map_err(|e| classify(&e))?;
    let status = resp.status().as_u16();
    let upstream_request_id = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    if !(200..300).contains(&status) {
        let retry_after_secs = crate::retry_after::seconds(resp.headers());
        let body = crate::openai::response_bytes(resp, Some(crate::limits::MAX_ERROR))
            .await
            .unwrap_or_default();
        return Err(UpstreamError::Status {
            status,
            body,
            retry_after_secs,
        });
    }

    if stream {
        let events = crate::limits::sse(resp)
            .eventsource()
            .map(|item| match item {
                Ok(event) => Ok(event.data),
                Err(e) => Err(UpstreamError::Stream(e.to_string())),
            });
        Ok(GeminiResponse::Stream(GeminiStream {
            upstream_request_id,
            events: Box::pin(events),
        }))
    } else {
        let body = crate::openai::response_bytes(resp, Some(crate::limits::MAX_BODY)).await?;
        Ok(GeminiResponse::Json {
            status,
            upstream_request_id,
            body,
        })
    }
}

/// 流中的 `error` 帧 → 上游错误。Gemini 的错误帧带 HTTP 同义的 `code`（缺省时看 `status`），
/// 映射回 `Status` 才能让 429 / 503 走限速冷却与退避，与 Anthropic 的 `error.type` 映射同理；
/// 体保留原帧。两样都认不出的仍按断流处理。
pub(crate) fn stream_error(err: &serde_json::Value, raw: &str) -> UpstreamError {
    let code = err
        .get("code")
        .and_then(serde_json::Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (400..=599).contains(code))
        .or_else(
            || match err.get("status").and_then(serde_json::Value::as_str)? {
                "RESOURCE_EXHAUSTED" => Some(429),
                "UNAVAILABLE" => Some(503),
                "INTERNAL" => Some(500),
                "DEADLINE_EXCEEDED" => Some(504),
                "INVALID_ARGUMENT" | "FAILED_PRECONDITION" => Some(400),
                "UNAUTHENTICATED" => Some(401),
                "PERMISSION_DENIED" => Some(403),
                "NOT_FOUND" => Some(404),
                _ => None,
            },
        );
    match code {
        Some(status) => UpstreamError::Status {
            status,
            body: bytes::Bytes::copy_from_slice(raw.as_bytes()),
            retry_after_secs: None,
        },
        None => UpstreamError::Stream(
            err.get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("upstream_error")
                .to_owned(),
        ),
    }
}

/// 透传形态（Gemini 入口 + Gemini 上游）的计费元数据扫描器：
/// chunk 原样透出（Gemini SSE 无 event 名），仅提取首字判定 / 字符数 / usage。
/// usage 口径与 `convert::openai_to_gemini::usage_from_gemini` 一致（promptTokenCount 含缓存，
/// completion = candidates + thoughts）；读取到 EOF，允许独立的尾部 usage 帧。
#[derive(Default)]
pub struct MetaScanner;

impl MetaScanner {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// 处理一条原生 chunk（data 行 JSON 原文）。
    pub fn scan(
        &mut self,
        item: Result<String, UpstreamError>,
    ) -> Vec<Result<crate::types::ChatEvent, UpstreamError>> {
        let raw = match item {
            Ok(raw) => raw,
            Err(err) => return vec![Err(err)],
        };
        let src: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => return vec![Err(UpstreamError::Stream("gemini_chunk_json".into()))],
        };
        if let Some(err) = src.get("error") {
            return vec![Err(stream_error(err, &raw))];
        }
        let mut has_output = false;
        let mut content_chars = 0usize;
        for part in src
            .pointer("/candidates/0/content/parts")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(t) = part.get("text").and_then(serde_json::Value::as_str) {
                if !t.is_empty() {
                    has_output = true;
                }
                content_chars = content_chars.saturating_add(t.chars().count());
            }
            if let Some(call) = part.get("functionCall") {
                has_output = true;
                // 工具参数也是产出：缺 usage 时按它估补全，与其它方言的流一致
                let args = call.get("args").map_or(0, |args| match args {
                    serde_json::Value::String(s) => s.chars().count(),
                    other => other.to_string().chars().count(),
                });
                content_chars = content_chars.saturating_add(args);
            }
        }
        let usage = crate::convert::openai_to_gemini::usage_from_gemini(src.get("usageMetadata"));
        let passthrough = crate::types::ChatEvent::Data {
            raw,
            event: None,
            has_output,
            content_chars,
            usage,
        };
        vec![Ok(passthrough)]
    }
}

fn classify(e: &reqwest::Error) -> UpstreamError {
    if let Some(unreachable) = UpstreamError::connect_phase(e) {
        unreachable
    } else if e.is_timeout() {
        UpstreamError::Timeout
    } else {
        UpstreamError::Stream(e.to_string())
    }
}

#[cfg(test)]
mod meta_scanner_tests {
    use super::*;

    #[test]
    fn stream_error_frames_keep_their_status() {
        let status =
            |frame: serde_json::Value| match MetaScanner::new().scan(Ok(frame.to_string())).pop() {
                Some(Err(UpstreamError::Status { status, body, .. })) => {
                    assert!(
                        String::from_utf8_lossy(&body).contains("\"error\""),
                        "保留原帧"
                    );
                    Some(status)
                }
                _ => None,
            };
        let frame = |error: serde_json::Value| serde_json::json!({ "error": error });
        assert_eq!(
            status(frame(serde_json::json!({"code": 429, "message": "m"}))),
            Some(429)
        );
        assert_eq!(
            status(frame(
                serde_json::json!({"status": "UNAVAILABLE", "message": "m"})
            )),
            Some(503)
        );
        assert_eq!(status(frame(serde_json::json!({"message": "m"}))), None);
    }

    #[test]
    fn function_call_arguments_count_as_generated_output() {
        let chunk = serde_json::json!({"candidates": [{"content": {"parts": [
            {"text": "ab"},
            {"functionCall": {"name": "read", "args": {"x": 1}}}
        ]}}]});
        let events = MetaScanner::new().scan(Ok(chunk.to_string()));
        let Some(Ok(crate::types::ChatEvent::Data {
            has_output,
            content_chars,
            ..
        })) = events.first()
        else {
            panic!("one passthrough event");
        };
        assert!(*has_output);
        assert_eq!(*content_chars, 2 + 7);
    }
}
