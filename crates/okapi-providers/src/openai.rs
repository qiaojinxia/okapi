//! OpenAI 方向上游客户端（原生 OpenAI 与一切 OpenAI 兼容上游共用）。

use crate::error::UpstreamError;
use crate::types::ChatEvent;
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use okapi_api::{ChunkProbe, UsageProbe};
use serde::Deserialize;
use std::pin::Pin;
use std::time::Duration;

/// 非流式请求总超时（流式不设总超时，首字窗口由 gateway 控制）。
const NON_STREAM_TIMEOUT: Duration = Duration::from_mins(2);

pub struct StreamHandle {
    pub upstream_request_id: Option<String>,
    pub events: Pin<Box<dyn Stream<Item = Result<ChatEvent, UpstreamError>> + Send>>,
}

pub enum ChatResponse {
    Stream(StreamHandle),
    Json {
        status: u16,
        upstream_request_id: Option<String>,
        body: Bytes,
        usage: Option<UsageProbe>,
    },
}

#[derive(Deserialize)]
struct UsageEnvelope {
    #[serde(default)]
    usage: Option<UsageProbe>,
}

/// 模型名映射重写：映射名一致时原样返回（透传零改动）。
pub fn rewrite_model(
    body: &Bytes,
    requested: &str,
    upstream_model: &str,
) -> Result<Bytes, UpstreamError> {
    if requested == upstream_model {
        return Ok(body.clone());
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(UpstreamError::Build("body_not_object".to_owned()));
    };
    obj.insert(
        "model".to_owned(),
        serde_json::Value::String(upstream_model.to_owned()),
    );
    let bytes = serde_json::to_vec(&value).map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok(Bytes::from(bytes))
}

#[derive(Clone)]
pub struct OpenAiUpstream {
    http: reqwest::Client,
}

impl OpenAiUpstream {
    /// 连接超时 client 级；读超时按流式/非流式分层（IMPLEMENTATION §1.1）。
    pub fn new() -> Result<Self, UpstreamError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| UpstreamError::Build(e.to_string()))?;
        Ok(Self { http })
    }

    /// 转发 chat completions。`body` 已完成模型名映射。
    pub async fn chat(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
        stream: bool,
    ) -> Result<ChatResponse, UpstreamError> {
        let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
        let mut req = self
            .http
            .post(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
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
                Ok(event) => Ok(parse_event(&event.data)),
                Err(e) => Err(UpstreamError::Stream(e.to_string())),
            });
            Ok(ChatResponse::Stream(StreamHandle {
                upstream_request_id,
                events: Box::pin(events),
            }))
        } else {
            let body = resp.bytes().await.map_err(|e| classify(&e))?;
            let usage = serde_json::from_slice::<UsageEnvelope>(&body)
                .ok()
                .and_then(|e| e.usage);
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            })
        }
    }
}

/// embeddings 上游响应（恒为 JSON）。
pub struct EmbeddingsResponse {
    pub status: u16,
    pub upstream_request_id: Option<String>,
    pub body: Bytes,
    pub usage: Option<UsageProbe>,
}

impl OpenAiUpstream {
    /// 转发 /v1/images/generations（`body` 已完成模型名映射；响应无 usage，媒体计费按张）。
    pub async fn images(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        self.post_json(api_base, "/images/generations", credential, body)
            .await
    }

    async fn post_json(
        &self,
        api_base: &str,
        path: &str,
        credential: &str,
        body: Bytes,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        let url = format!("{}{path}", api_base.trim_end_matches('/'));
        let resp = self
            .http
            .post(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .timeout(NON_STREAM_TIMEOUT)
            .send()
            .await
            .map_err(|e| classify(&e))?;
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
        let body = resp.bytes().await.map_err(|e| classify(&e))?;
        let usage = serde_json::from_slice::<UsageEnvelope>(&body)
            .ok()
            .and_then(|e| e.usage);
        Ok(EmbeddingsResponse {
            status,
            upstream_request_id,
            body,
            usage,
        })
    }

    /// 转发 /v1/embeddings（`body` 已完成模型名映射）。
    pub async fn embeddings(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        self.post_json(api_base, "/embeddings", credential, body)
            .await
    }

    /// /v1/audio/speech：JSON 入、二进制音频出（bytes 整体返回）。
    pub async fn speech(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
    ) -> Result<(u16, String, Bytes), UpstreamError> {
        let url = format!("{}/audio/speech", api_base.trim_end_matches('/'));
        let resp = self
            .http
            .post(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .timeout(NON_STREAM_TIMEOUT)
            .send()
            .await
            .map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.bytes().await.unwrap_or_default();
            return Err(UpstreamError::Status {
                status,
                body,
                retry_after_secs: None,
            });
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("audio/mpeg")
            .to_owned();
        let body = resp.bytes().await.map_err(|e| classify(&e))?;
        Ok((status, content_type, body))
    }

    /// /v1/audio/transcriptions：multipart 入、JSON 出。
    /// parts = (name, filename?, content_type?, data)；boundary 由 reqwest 重生成。
    pub async fn transcriptions(
        &self,
        api_base: &str,
        credential: &str,
        parts: Vec<(String, Option<String>, Option<String>, Bytes)>,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        self.audio_multipart(api_base, "/audio/transcriptions", credential, parts)
            .await
    }

    /// 音频 multipart 通用转发（transcriptions / translations 同构，老 ok-api 面核对补）。
    pub async fn audio_multipart(
        &self,
        api_base: &str,
        path: &str,
        credential: &str,
        parts: Vec<(String, Option<String>, Option<String>, Bytes)>,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        let url = format!("{}{path}", api_base.trim_end_matches('/'));
        let mut form = reqwest::multipart::Form::new();
        for (name, filename, content_type, data) in parts {
            let base = |data: &Bytes, filename: &Option<String>| {
                let mut part = reqwest::multipart::Part::bytes(data.to_vec());
                if let Some(f) = filename {
                    part = part.file_name(f.clone());
                }
                part
            };
            let mut part = base(&data, &filename);
            if let Some(ct) = &content_type
                && let Ok(p) = base(&data, &filename).mime_str(ct)
            {
                // mime 非法时静默忽略 content-type（保数据与文件名）
                part = p;
            }
            form = form.part(name, part);
        }
        let resp = self
            .http
            .post(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
            .multipart(form)
            .timeout(NON_STREAM_TIMEOUT)
            .send()
            .await
            .map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        let upstream_request_id = resp
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if !(200..300).contains(&status) {
            let body = resp.bytes().await.unwrap_or_default();
            return Err(UpstreamError::Status {
                status,
                body,
                retry_after_secs: None,
            });
        }
        let body = resp.bytes().await.map_err(|e| classify(&e))?;
        Ok(EmbeddingsResponse {
            status,
            upstream_request_id,
            body,
            usage: None,
        })
    }

    /// 通用非流式 JSON 中继（rerank 等 OpenAI 兼容衍生端点）。
    pub async fn json_relay(
        &self,
        api_base: &str,
        path: &str,
        credential: &str,
        body: Bytes,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        self.post_json(api_base, path, credential, body).await
    }

    /// 转发 POST /v1/videos（异步任务创建；`body` 已完成模型名映射）。
    pub async fn videos_create(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        self.post_json(api_base, "/videos", credential, body).await
    }

    /// GET JSON 中继（videos 任务轮询等只读小体积端点）。
    pub async fn get_json(
        &self,
        api_base: &str,
        path: &str,
        credential: &str,
    ) -> Result<EmbeddingsResponse, UpstreamError> {
        let url = format!("{}{path}", api_base.trim_end_matches('/'));
        let resp = self
            .http
            .get(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
            .timeout(NON_STREAM_TIMEOUT)
            .send()
            .await
            .map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        let upstream_request_id = resp
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = resp.bytes().await.map_err(|e| classify(&e))?;
        Ok(EmbeddingsResponse {
            status,
            upstream_request_id,
            body,
            usage: None,
        })
    }

    /// GET 流式中继（videos content 下载等大体积端点）：
    /// 返回原始响应由调用方消费字节流，避免整段缓冲。
    pub async fn get_stream(
        &self,
        api_base: &str,
        path: &str,
        credential: &str,
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = format!("{}{path}", api_base.trim_end_matches('/'));
        self.http
            .get(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {credential}"),
            )
            .send()
            .await
            .map_err(|e| classify(&e))
    }
}

fn parse_event(data: &str) -> ChatEvent {
    if data.trim() == "[DONE]" {
        return ChatEvent::Done;
    }
    match serde_json::from_str::<ChunkProbe>(data) {
        Ok(probe) => ChatEvent::Data {
            event: None,
            has_output: probe.has_output(),
            content_chars: probe.content_chars(),
            usage: probe.usage,
            raw: data.to_owned(),
        },
        // 未知负载：透传但不参与首字/计数判定
        Err(_) => ChatEvent::Data {
            event: None,
            has_output: false,
            content_chars: 0,
            usage: None,
            raw: data.to_owned(),
        },
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
