//! custom_pass 透传传输（IMPLEMENTATION §4.1 模块位 / §4.4 语义）：
//! 任意方法 + 路径的透明代理，响应体流式回传；协议语义与计费由 gateway 决策。

use crate::error::UpstreamError;
use crate::http::{HttpPool, Outbound};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::pin::Pin;
use std::time::Duration;

const PASS_TIMEOUT: Duration = Duration::from_mins(5);

pub struct PassRequest {
    pub method: reqwest::Method,
    pub url: String,
    /// 上游凭证头（如 authorization / x-api-key）。
    pub auth_header: String,
    pub auth_value: String,
    pub content_type: Option<String>,
    pub body: Bytes,
    pub proxy_url: Option<String>,
    pub extra_headers: Vec<(String, String)>,
}

pub enum PassResponse {
    /// 2xx：响应体流式转交（大文件不落内存）。
    Ok {
        status: u16,
        content_type: String,
        stream: Pin<Box<dyn Stream<Item = Result<Bytes, UpstreamError>> + Send>>,
    },
    /// 非 2xx：错误体整体读出（透传给客户端）。
    ErrStatus { status: u16, body: Bytes },
}

#[derive(Clone)]
pub struct PassUpstream {
    http: HttpPool,
}

impl PassUpstream {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            http: HttpPool::new()?,
        })
    }

    /// 数据面透传：跟随重定向（reqwest 缺省）。
    pub async fn forward(&self, req: PassRequest) -> Result<PassResponse, UpstreamError> {
        self.send(req, false).await
    }

    /// 管理面探针（测活 / 拉模型 / 余额 / OAuth / Turnstile / 支付）：**不跟随重定向**，
    /// 否则 SSRF 闸校验过的公网地址一跳 30x 就能把请求引到私网（见 `HttpPool` 说明）。
    pub async fn probe(&self, req: PassRequest) -> Result<PassResponse, UpstreamError> {
        self.send(req, true).await
    }

    async fn send(&self, req: PassRequest, probe: bool) -> Result<PassResponse, UpstreamError> {
        let outbound = Outbound {
            proxy_url: req.proxy_url,
            extra_headers: req.extra_headers,
        };
        let builder = if probe {
            self.http.probe(&outbound, req.method, req.url)?
        } else {
            self.http.request(&outbound, req.method, req.url)?
        };
        let mut builder = builder
            .timeout(PASS_TIMEOUT)
            .header(req.auth_header.as_str(), req.auth_value.as_str());
        if let Some(ct) = &req.content_type {
            builder = builder.header(reqwest::header::CONTENT_TYPE, ct.as_str());
        }
        if !req.body.is_empty() {
            builder = builder.body(req.body.to_vec());
        }

        let resp = builder.send().await.map_err(|e| classify(&e))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_owned();
            let stream = resp
                .bytes_stream()
                .map(|item| item.map_err(|e| UpstreamError::Stream(e.to_string())));
            Ok(PassResponse::Ok {
                status,
                content_type,
                stream: Box::pin(stream),
            })
        } else {
            let body = resp.bytes().await.unwrap_or_default();
            Ok(PassResponse::ErrStatus { status, body })
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
