//! Anthropic 原生上游客户端（/v1/messages）。
//!
//! 只负责传输与事件切分：返回原生 Anthropic SSE 事件（event 名 + data 原文），
//! 协议转换在 convert 模块按方向进行（禁统一 IR，IMPLEMENTATION §4.1）。
//! 原生事件保留原文，为后续 /v1/messages 入口 + anthropic 上游的纯透传留路。

use crate::error::UpstreamError;
use crate::types::ChatEvent;
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use okapi_api::UsageProbe;
use std::pin::Pin;
use std::time::Duration;

const NON_STREAM_TIMEOUT: Duration = Duration::from_mins(2);
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// 原生 Anthropic SSE 事件（event 行 + data 行原文）。
#[derive(Debug, Clone)]
pub struct AnthropicEvent {
    pub event: String,
    pub data: String,
}

pub struct MessagesStream {
    pub upstream_request_id: Option<String>,
    pub events: Pin<Box<dyn Stream<Item = Result<AnthropicEvent, UpstreamError>> + Send>>,
}

pub enum MessagesResponse {
    Stream(MessagesStream),
    Json {
        status: u16,
        upstream_request_id: Option<String>,
        body: Bytes,
    },
}

#[derive(Clone)]
pub struct AnthropicUpstream {
    http: reqwest::Client,
}

impl AnthropicUpstream {
    pub fn new() -> Result<Self, UpstreamError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| UpstreamError::Build(e.to_string()))?;
        Ok(Self { http })
    }

    /// 转发 /v1/messages。`body` 已是 Anthropic 协议 JSON（含 stream 字段）。
    pub async fn messages(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<MessagesResponse, UpstreamError> {
        let url = format!("{}/messages", api_base.trim_end_matches('/'));
        let mut req = self
            .http
            .post(url)
            .header("x-api-key", credential)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec());
        if !stream {
            req = req.timeout(NON_STREAM_TIMEOUT);
        }

        let resp = req.send().await.map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        let upstream_request_id = resp
            .headers()
            .get("request-id")
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
                Ok(event) => Ok(AnthropicEvent {
                    event: event.event,
                    data: event.data,
                }),
                Err(e) => Err(UpstreamError::Stream(e.to_string())),
            });
            Ok(MessagesResponse::Stream(MessagesStream {
                upstream_request_id,
                events: Box::pin(events),
            }))
        } else {
            let body = resp.bytes().await.map_err(|e| classify(&e))?;
            Ok(MessagesResponse::Json {
                status,
                upstream_request_id,
                body,
            })
        }
    }
}

/// 透传形态（Anthropic 入口 + Anthropic 上游）的计费元数据扫描器：
/// 事件原样透出（保留 event 名），仅提取首字判定 / 字符数 / usage。
/// usage 口径与 convert::openai_to_anthropic 一致：prompt 含缓存读写，cached=cache_read。
#[derive(Default)]
pub struct MetaScanner {
    input_tokens: u32,
    cache_read: u32,
    cache_creation: u32,
}

impl MetaScanner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 处理一条原生事件，产出透传事件（message_stop 后追加流终止标记）。
    pub fn scan(
        &mut self,
        item: Result<AnthropicEvent, UpstreamError>,
    ) -> Vec<Result<ChatEvent, UpstreamError>> {
        let ev = match item {
            Ok(ev) => ev,
            Err(err) => return vec![Err(err)],
        };
        let data: serde_json::Value = serde_json::from_str(&ev.data).unwrap_or_default();
        let mut has_output = false;
        let mut content_chars = 0usize;
        let mut usage: Option<UsageProbe> = None;
        match ev.event.as_str() {
            "message_start" => {
                let get = |k: &str| {
                    data.get("message")
                        .and_then(|m| m.get("usage"))
                        .and_then(|u| u.get(k))
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0)
                };
                self.input_tokens = get("input_tokens");
                self.cache_read = get("cache_read_input_tokens");
                self.cache_creation = get("cache_creation_input_tokens");
            }
            "content_block_start" => {
                has_output = data
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("tool_use");
            }
            "content_block_delta" => {
                has_output = true;
                content_chars = data.get("delta").map_or(0, |d| {
                    ["text", "thinking", "partial_json"]
                        .iter()
                        .filter_map(|k| d.get(k).and_then(serde_json::Value::as_str))
                        .map(|s| s.chars().count())
                        .sum()
                });
            }
            "message_delta" => {
                let output = data
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(0);
                usage = Some(UsageProbe {
                    prompt_tokens: self
                        .input_tokens
                        .saturating_add(self.cache_read)
                        .saturating_add(self.cache_creation),
                    completion_tokens: output,
                    prompt_tokens_details: okapi_api::PromptTokensDetails {
                        cached_tokens: self.cache_read,
                        // 缓存写入独立成段：官方 1.25×@5m TTL，混入常规输入段会漏计费
                        cache_write_tokens: self.cache_creation,
                    },
                    completion_tokens_details: okapi_api::CompletionTokensDetails {
                        reasoning_tokens: 0,
                    },
                });
            }
            "error" => {
                let msg = data
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("upstream_error");
                return vec![Err(UpstreamError::Stream(msg.to_owned()))];
            }
            _ => {}
        }
        let passthrough = ChatEvent::Data {
            raw: ev.data,
            event: Some(ev.event.clone()),
            has_output,
            content_chars,
            usage,
        };
        if ev.event == "message_stop" {
            vec![Ok(passthrough), Ok(ChatEvent::Done)]
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
