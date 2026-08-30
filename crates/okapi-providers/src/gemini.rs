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
    http: reqwest::Client,
}

impl GeminiUpstream {
    pub fn new() -> Result<Self, UpstreamError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| UpstreamError::Build(e.to_string()))?;
        Ok(Self { http })
    }

    /// 转发 generateContent。`body` 已是 Gemini 协议 JSON；模型名走 URL 路径。
    pub async fn generate(
        &self,
        api_base: &str,
        credential: &str,
        model: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<GeminiResponse, UpstreamError> {
        let base = api_base.trim_end_matches('/');
        let url = if stream {
            format!("{base}/models/{model}:streamGenerateContent?alt=sse")
        } else {
            format!("{base}/models/{model}:generateContent")
        };
        let mut req = self
            .http
            .post(url)
            .header("x-goog-api-key", credential)
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
