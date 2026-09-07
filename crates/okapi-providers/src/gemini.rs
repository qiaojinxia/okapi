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
        .body(body.to_vec());
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
        let events = resp.bytes_stream().eventsource().map(|item| match item {
            Ok(event) => Ok(event.data),
            Err(e) => Err(UpstreamError::Stream(e.to_string())),
        });
        Ok(GeminiResponse::Stream(GeminiStream {
            upstream_request_id,
            events: Box::pin(events),
        }))
    } else {
        let body = resp.bytes().await.map_err(|e| classify(&e))?;
        Ok(GeminiResponse::Json {
            status,
            upstream_request_id,
            body,
        })
    }
}

/// 透传形态（Gemini 入口 + Gemini 上游）的计费元数据扫描器：
/// chunk 原样透出（Gemini SSE 无 event 名），仅提取首字判定 / 字符数 / usage。
/// usage 口径与 `convert::openai_to_gemini::usage_from_gemini` 一致（promptTokenCount 含缓存，
/// completion = candidates + thoughts）；`finishReason` 出现即终局，其后追加流终止标记。
#[derive(Default)]
pub struct MetaScanner {
    finished: bool,
}

impl MetaScanner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        let src: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        if let Some(err) = src.get("error") {
            let msg = err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("upstream_error");
            return vec![Err(UpstreamError::Stream(msg.to_owned()))];
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
            if part.get("functionCall").is_some() {
                has_output = true;
            }
        }
        // usageMetadata 逐 chunk 累计给出；只在终局 chunk 上交给结算，避免中途值覆盖
        let finished = src
            .pointer("/candidates/0/finishReason")
            .and_then(serde_json::Value::as_str)
            .is_some();
        let usage = if finished {
            Some(crate::convert::openai_to_gemini::usage_from_gemini(
                src.get("usageMetadata"),
            ))
        } else {
            None
        };
        let passthrough = crate::types::ChatEvent::Data {
            raw,
            event: None,
            has_output,
            content_chars,
            usage,
        };
        if finished && !self.finished {
            self.finished = true;
            vec![Ok(passthrough), Ok(crate::types::ChatEvent::Done)]
        } else {
            vec![Ok(passthrough)]
        }
    }
}

fn classify(e: &reqwest::Error) -> UpstreamError {
    if e.is_timeout() {
        UpstreamError::Timeout
    } else if e.is_connect() {
        UpstreamError::Connect(e.to_string())
    } else {
        UpstreamError::Stream(e.to_string())
    }
}
