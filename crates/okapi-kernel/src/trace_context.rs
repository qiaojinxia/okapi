//! W3C Trace Context（`traceparent`）解析、生成与透传中间件。
//!
//! 格式：`00-<32-hex-trace-id>-<16-hex-parent-id>-<2-hex-flags>`。
//! - 入口带合法 `traceparent` 时继承 `trace_id` 与 `flags`，为本跳签发新的 `span_id`；
//! - 缺失或非法时生成新的 `trace_id` / `span_id`（`flags = 01`）；
//! - 将当前跳的 `traceparent` 写入请求头、请求扩展与响应头，供 `request_span`、
//!   `diagnostics::Trace` 与内部异步接力（outbox → NATS → worker）贯通关联。

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

pub const TRACEPARENT_HEADER: &str = "traceparent";

/// 解析或签发后的 W3C Trace Context。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub flags: u8,
}

impl TraceContext {
    /// 从入口请求头提取或签发新的追踪上下文。
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let parsed = headers
            .get(TRACEPARENT_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_traceparent);
        let span_id = new_span_id();
        match parsed {
            Some((trace_id, parent_span_id, flags)) => Self {
                trace_id,
                span_id,
                parent_span_id: Some(parent_span_id),
                flags,
            },
            None => Self {
                trace_id: new_trace_id(),
                span_id,
                parent_span_id: None,
                flags: 0x01,
            },
        }
    }

    /// 格式化为标准 W3C `traceparent` 头值。
    #[must_use]
    pub fn header_value(&self) -> String {
        format!("00-{}-{}-{:02x}", self.trace_id, self.span_id, self.flags)
    }
}

/// 解析 W3C `traceparent`（仅接受 version `00` 与非全零十六进制 ID）。
#[must_use]
pub fn parse_traceparent(raw: &str) -> Option<(String, String, u8)> {
    let mut parts = raw.trim().split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let parent_id = parts.next()?;
    let flags_hex = parts.next()?;
    if parts.next().is_some() || version != "00" {
        return None;
    }
    if trace_id.len() != 32 || parent_id.len() != 16 || flags_hex.len() != 2 {
        return None;
    }
    if !is_nonzero_hex(trace_id) || !is_nonzero_hex(parent_id) {
        return None;
    }
    let flags = u8::from_str_radix(flags_hex, 16).ok()?;
    Some((
        trace_id.to_ascii_lowercase(),
        parent_id.to_ascii_lowercase(),
        flags,
    ))
}

fn is_nonzero_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_hexdigit()) && s.bytes().any(|b| b != b'0')
}

fn new_trace_id() -> String {
    let id = Uuid::new_v4().simple().to_string();
    if is_nonzero_hex(&id) {
        id
    } else {
        "00000000000000000000000000000001".to_owned()
    }
}

fn new_span_id() -> String {
    let bytes = Uuid::new_v4().into_bytes();
    let hex = hex::encode(&bytes[..8]);
    if is_nonzero_hex(&hex) {
        hex
    } else {
        "0000000000000001".to_owned()
    }
}

/// Axum 中间件：在 `TraceLayer` 之前执行，确保 `request_span`、handler 与响应头共享同一 `traceparent`。
pub async fn propagate(mut req: Request, next: Next) -> Response {
    let ctx = TraceContext::from_headers(req.headers());
    let header_text = ctx.header_value();
    if let Ok(value) = HeaderValue::from_str(&header_text) {
        req.headers_mut()
            .insert(HeaderName::from_static(TRACEPARENT_HEADER), value);
    }
    req.extensions_mut().insert(ctx);
    let mut resp = next.run(req).await;
    if !resp.headers().contains_key(TRACEPARENT_HEADER)
        && let Ok(value) = HeaderValue::from_str(&header_text)
    {
        resp.headers_mut()
            .insert(HeaderName::from_static(TRACEPARENT_HEADER), value);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_w3c_traceparent_and_rejects_malformed_inputs() {
        let parsed = parse_traceparent("00-4BF92F3577B34DA6A3CE929D0E0E4736-00F067AA0BA902B7-01")
            .expect("valid traceparent");
        assert_eq!(
            parsed,
            (
                "4bf92f3577b34da6a3ce929d0e0e4736".to_owned(),
                "00f067aa0ba902b7".to_owned(),
                1
            )
        );
        // All-zero trace_id / parent_id, wrong version, wrong lengths are rejected.
        assert!(
            parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none()
        );
        assert!(
            parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01").is_none()
        );
        assert!(
            parse_traceparent("ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none()
        );
        assert!(parse_traceparent("00-short-00f067aa0ba902b7-01").is_none());
        assert!(
            parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra")
                .is_none()
        );
    }

    #[test]
    fn from_headers_preserves_trace_id_and_mints_new_span_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT_HEADER,
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00"),
        );
        let ctx = TraceContext::from_headers(&headers);
        assert_eq!(ctx.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(ctx.parent_span_id.as_deref(), Some("00f067aa0ba902b7"));
        assert_ne!(ctx.span_id, "00f067aa0ba902b7");
        assert_eq!(ctx.flags, 0);
        assert!(parse_traceparent(&ctx.header_value()).is_some());

        let fresh = TraceContext::from_headers(&HeaderMap::new());
        assert!(fresh.parent_span_id.is_none());
        assert_eq!(fresh.flags, 1);
        assert!(parse_traceparent(&fresh.header_value()).is_some());
    }
}
