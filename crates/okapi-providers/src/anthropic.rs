//! Anthropic 原生上游客户端（/v1/messages）。
//!
//! 只负责传输与事件切分：返回原生 Anthropic SSE 事件（event 名 + data 原文），
//! 协议转换在 convert 模块按方向进行（禁统一 IR，IMPLEMENTATION §4.1）。
//! 原生事件保留原文，为后续 /v1/messages 入口 + anthropic 上游的纯透传留路。

use crate::beta_latch::{beta_parts, drop_refused, refusal_retry, secret_scope};
use crate::error::UpstreamError;
use crate::types::ChatEvent;
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use std::pin::Pin;
use std::time::Duration;

const NON_STREAM_TIMEOUT: Duration = Duration::from_mins(2);
/// `anthropic-version` 头的值：真实请求与管理面的测活 / 拉模型共用，探测才和流量走同一种协议。
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// 转给上游的客户端 beta 最多几个、名字最长几个字符；形状不对的直接丢弃（只放行形如 `prompt-caching-2024-07-31` 的名字）。
const MAX_FORWARDED_BETAS: usize = 32;
const MAX_BETA_NAME: usize = 64;

fn beta_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_BETA_NAME
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

/// API key 路径上这条请求要发的 `anthropic-beta`，以及拒收记录的归属（按这把 key）。
///
/// 没有客户端画像（模拟 Claude Code）接管时：渠道自己配的 `anthropic-beta`（`extra_headers`）在前，客户端带来的随后，去重；
/// 渠道配的那一行从 `extra_headers` 里摘走，免得发出两行同名头。画像接管时整件事一个字不动（由画像自己管 beta）。
/// 渠道自己配的永远不被自愈丢掉——那是站长明说要发的。
struct BetaPlan {
    value: String,
    scope: String,
    configured: Vec<String>,
}

impl BetaPlan {
    fn new(prepared: &mut crate::profiles::PreparedRequest, credential: &str) -> Option<Self> {
        if prepared.shaped {
            return None;
        }
        let outbound = &mut prepared.outbound;
        let configured = outbound
            .extra_headers
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
            .map(|index| outbound.extra_headers.remove(index).1);
        let client = outbound
            .context
            .client_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
            .map(|(_, value)| value.as_str());
        let configured: Vec<String> = configured
            .iter()
            .flat_map(|value| beta_parts(value))
            .map(str::to_owned)
            .collect();
        let mut parts = configured.clone();
        for part in client
            .into_iter()
            .flat_map(beta_parts)
            .filter(|part| beta_name_ok(part))
        {
            if parts.len() >= MAX_FORWARDED_BETAS {
                break;
            }
            if !parts.iter().any(|existing| existing == part) {
                parts.push(part.to_owned());
            }
        }
        let scope = secret_scope("key", credential);
        let required: Vec<&str> = configured.iter().map(String::as_str).collect();
        let value = drop_refused(&scope, &parts.join(","), &required);
        Some(Self {
            value,
            scope,
            configured,
        })
    }

    /// 上游点名拒收了其中某些 beta：返回去掉它们的 beta 串，调用方换上重发一次。
    fn refusal<T>(&self, result: &Result<T, UpstreamError>, retried: &mut bool) -> Option<String> {
        let required: Vec<&str> = self.configured.iter().map(String::as_str).collect();
        refusal_retry(&self.scope, &self.value, &required, result, retried)
    }
}

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
    http: crate::http::HttpPool,
}

impl AnthropicUpstream {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            http: crate::http::HttpPool::new()?,
        })
    }

    /// 共享连接池（`anthropic_max` 订阅路径复用，§11.38）。
    #[must_use]
    pub fn http(&self) -> &crate::http::HttpPool {
        &self.http
    }

    /// 转发 /v1/messages。`body` 已是 Anthropic 协议 JSON（含 stream 字段）。
    pub async fn messages(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
        stream: bool,
        outbound: &crate::http::Outbound,
    ) -> Result<MessagesResponse, UpstreamError> {
        let url = format!("{}/messages", api_base.trim_end_matches('/'));
        let mut prepared = crate::profiles::prepare_anthropic(body, stream, false, outbound, None)?;
        let mut plan = BetaPlan::new(&mut prepared, credential);
        let mut retried = false;
        loop {
            let mut headers = vec![
                ("x-api-key", credential),
                ("anthropic-version", ANTHROPIC_VERSION),
            ];
            if let Some(plan) = plan.as_ref().filter(|plan| !plan.value.is_empty()) {
                headers.push(("anthropic-beta", plan.value.as_str()));
            }
            headers.extend(
                prepared
                    .headers
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
            let result = send_messages_at(
                &self.http,
                url.clone(),
                &headers,
                prepared.body.clone(),
                stream,
                &prepared.outbound,
            )
            .await;
            if let Some(next) = plan
                .as_ref()
                .and_then(|plan| plan.refusal(&result, &mut retried))
                && let Some(plan) = plan.as_mut()
            {
                plan.value = next;
                continue;
            }
            return result;
        }
    }

    /// `POST /v1/messages/count_tokens`：不计费，原样回 `{input_tokens}`。
    pub async fn count_tokens(
        &self,
        api_base: &str,
        credential: &str,
        body: Bytes,
        outbound: &crate::http::Outbound,
    ) -> Result<Bytes, UpstreamError> {
        let url = format!("{}/messages/count_tokens", api_base.trim_end_matches('/'));
        let mut prepared = crate::profiles::prepare_anthropic(body, false, true, outbound, None)?;
        let mut plan = BetaPlan::new(&mut prepared, credential);
        let mut retried = false;
        loop {
            let mut req = self
                .http
                .post(&prepared.outbound, url.clone())?
                .header("x-api-key", credential)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .timeout(NON_STREAM_TIMEOUT)
                .body(prepared.body.clone());
            if let Some(plan) = plan.as_ref().filter(|plan| !plan.value.is_empty()) {
                req = req.header("anthropic-beta", plan.value.as_str());
            }
            for (name, value) in &prepared.headers {
                req = req.header(name.as_str(), value.as_str());
            }
            let resp = self.http.send(req, &prepared.outbound).await?;
            let status = resp.status().as_u16();
            let result = if (200..300).contains(&status) {
                crate::openai::response_bytes(resp, Some(crate::limits::MAX_BODY)).await
            } else {
                Err(crate::openai::status_error(resp).await)
            };
            if let Some(next) = plan
                .as_ref()
                .and_then(|plan| plan.refusal(&result, &mut retried))
                && let Some(plan) = plan.as_mut()
            {
                plan.value = next;
                continue;
            }
            return result;
        }
    }
}

/// 429 的冷却时长：`Retry-After` 优先；订阅路径的 429 只带 `anthropic-ratelimit-unified-reset`
/// （unix 秒，5h/7d 窗口的重置点），据它推出剩余秒数（§11.38；API key 响应没有这个头）。
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    crate::retry_after::seconds(headers)
}

/// 向任意 URL 发一次 Anthropic Messages 形状的请求（鉴权头由调用方给）：直连官方走
/// `x-api-key`，Vertex rawPredict 走 Bearer（IMPLEMENTATION §11.35）。流式响应按 SSE 切事件，
/// 非流式整读；两者的错误分类与请求 ID 提取同一份。
pub async fn send_messages_at(
    http: &crate::http::HttpPool,
    url: String,
    auth_headers: &[(&str, &str)],
    body: Bytes,
    stream: bool,
    outbound: &crate::http::Outbound,
) -> Result<MessagesResponse, UpstreamError> {
    let mut req = http
        .post(outbound, url)?
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.clone());
    for (name, value) in auth_headers {
        req = req.header(*name, *value);
    }
    if !stream {
        req = req.timeout(NON_STREAM_TIMEOUT);
    }

    let resp = http.send(req, outbound).await?;
    let status = resp.status().as_u16();
    // 订阅账号的额度窗口头：成功与失败的响应都报给网关挂上的观测钩子（没挂则无事发生）
    crate::unified_limits::report(status, resp.headers());
    let upstream_request_id = ["request-id", "x-request-id"]
        .iter()
        .find_map(|h| resp.headers().get(*h))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    if !(200..300).contains(&status) {
        let retry_after_secs = retry_after_secs(resp.headers());
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
        let body = crate::openai::response_bytes(resp, Some(crate::limits::MAX_BODY)).await?;
        Ok(MessagesResponse::Json {
            status,
            upstream_request_id,
            body,
        })
    }
}

/// 透传形态（Anthropic 入口 + Anthropic 上游）的计费元数据扫描器：
/// 事件原样透出（保留 event 名），仅提取首字判定 / 字符数 / usage。
/// usage 口径与 convert::openai_to_anthropic 一致：prompt 含缓存读写，cached=cache_read。
#[derive(Default)]
pub struct MetaScanner {
    usage: crate::anthropic_usage::StreamUsage,
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
        // 按事件名分派：95%+ 的 content_block_delta / content_block_start 走零 DOM 借用探针，
        // ping / content_block_stop 直接跳过 JSON 解析；仅含 usage/error 的极少数事件解析 Value。
        let (has_output, content_chars, usage) = match ev.event.as_str() {
            "content_block_start" => {
                #[derive(serde::Deserialize)]
                struct BlockStart<'a> {
                    #[serde(default, borrow)]
                    content_block: Option<BlockKind<'a>>,
                }
                #[derive(serde::Deserialize)]
                struct BlockKind<'a> {
                    #[serde(
                        default,
                        borrow,
                        rename = "type",
                        deserialize_with = "crate::json_probe::opt_str"
                    )]
                    kind: Option<std::borrow::Cow<'a, str>>,
                }
                let is_tool_use = serde_json::from_str::<BlockStart>(&ev.data)
                    .ok()
                    .and_then(|s| s.content_block)
                    .and_then(|b| b.kind)
                    .is_some_and(|k| k == "tool_use");
                (is_tool_use, 0, None)
            }
            "content_block_delta" => {
                #[derive(serde::Deserialize)]
                struct BlockDelta<'a> {
                    #[serde(default, borrow)]
                    delta: Option<DeltaText<'a>>,
                }
                #[derive(serde::Deserialize)]
                struct DeltaText<'a> {
                    #[serde(default, borrow, deserialize_with = "crate::json_probe::opt_str")]
                    text: Option<std::borrow::Cow<'a, str>>,
                    #[serde(default, borrow, deserialize_with = "crate::json_probe::opt_str")]
                    thinking: Option<std::borrow::Cow<'a, str>>,
                    #[serde(default, borrow, deserialize_with = "crate::json_probe::opt_str")]
                    partial_json: Option<std::borrow::Cow<'a, str>>,
                }
                let chars = serde_json::from_str::<BlockDelta>(&ev.data)
                    .ok()
                    .and_then(|d| d.delta)
                    .map_or(0, |d| {
                        d.text.as_deref().map_or(0, |s| s.chars().count())
                            + d.thinking.as_deref().map_or(0, |s| s.chars().count())
                            + d.partial_json.as_deref().map_or(0, |s| s.chars().count())
                    });
                // 只有带正文 / 思考 / 工具参数的增量才算产出；signature_delta 之类的元数据不算，
                // 否则一条只发签名的畸形流就能绕过空回复判定
                (chars > 0, chars, None)
            }
            "message_start" | "message_delta" | "message_stop" => {
                let data: serde_json::Value = serde_json::from_str(&ev.data).unwrap_or_default();
                (false, 0, self.usage.observe(&ev.event, &data))
            }
            "error" => {
                let data: serde_json::Value = serde_json::from_str(&ev.data).unwrap_or_default();
                return vec![Err(stream_error(&data, ev.data.as_bytes()))];
            }
            _ => (false, 0, None),
        };
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

/// 流中的 `error` 事件 → 上游错误。`error.type` 与同义的 HTTP 状态对齐（`overloaded_error`=529、
/// `rate_limit_error`=429、`api_error`=500 …），重试矩阵与 key 冷却（529 退避、429 限速）才能分流；
/// 体保留原事件，首字前失败时客户端拿到上游原样的 Anthropic 错误。认不出的类型仍按断流处理。
pub(crate) fn stream_error(data: &serde_json::Value, raw: &[u8]) -> UpstreamError {
    let error = data.get("error");
    let status = match error
        .and_then(|e| e.get("type"))
        .and_then(serde_json::Value::as_str)
    {
        Some("overloaded_error") => Some(529),
        Some("rate_limit_error") => Some(429),
        Some("api_error") => Some(500),
        Some("timeout_error") => Some(504),
        Some("invalid_request_error") => Some(400),
        Some("authentication_error") => Some(401),
        Some("permission_error") => Some(403),
        Some("not_found_error") => Some(404),
        Some("request_too_large") => Some(413),
        _ => None,
    };
    match status {
        Some(status) => UpstreamError::Status {
            status,
            body: bytes::Bytes::copy_from_slice(raw),
            retry_after_secs: None,
        },
        None => UpstreamError::Stream(
            error
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("upstream_error")
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn retry_after_prefers_header_then_unified_reset() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after_secs(&headers), None);

        let reset = chrono::Utc::now().timestamp() + 1800;
        headers.insert(
            "anthropic-ratelimit-unified-reset",
            HeaderValue::from_str(&reset.to_string()).unwrap(),
        );
        let derived = retry_after_secs(&headers).unwrap();
        assert!((1795..=1800).contains(&derived), "{derived}");

        headers.insert("retry-after", HeaderValue::from_static("30"));
        assert_eq!(retry_after_secs(&headers), Some(30));

        let mut past = HeaderMap::new();
        past.insert(
            "anthropic-ratelimit-unified-reset",
            HeaderValue::from_static("1"),
        );
        assert_eq!(retry_after_secs(&past), None, "已过去的重置点不冷却");
    }
}

#[cfg(test)]
mod stream_error_tests {
    use super::stream_error;
    use crate::UpstreamError;
    use serde_json::json;

    #[test]
    fn stream_error_keeps_the_anthropic_error_type() {
        let status = |kind: &str| {
            let data = json!({"type":"error","error":{"type":kind,"message":"m"}});
            match stream_error(&data, data.to_string().as_bytes()) {
                UpstreamError::Status { status, body, .. } => {
                    assert!(
                        String::from_utf8_lossy(&body).contains(kind),
                        "保留原事件体"
                    );
                    Some(status)
                }
                _ => None,
            }
        };
        assert_eq!(status("overloaded_error"), Some(529));
        assert_eq!(status("rate_limit_error"), Some(429));
        assert_eq!(status("api_error"), Some(500));
        assert_eq!(status("something_new"), None);
        assert!(matches!(
            stream_error(&json!({"error":{"message":"boom"}}), b""),
            UpstreamError::Stream(m) if m == "boom"
        ));
    }
}
