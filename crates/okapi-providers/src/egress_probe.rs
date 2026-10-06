//! 出口代理测试（IMPLEMENTATION §11.41）：经代理请求一个探测地址，量延迟、拿出口 IP。
//!
//! 缺省探测 Cloudflare 的 `/cdn-cgi/trace`：一次请求同时给出出口 IP（`ip=`）与国家（`loc=`）。
//! 探测地址可换（只放行特定上游的企业代理、内网环境）：拿到任何 HTTP 响应都算「经代理可达」，
//! 解析不出出口 IP 只是少了展示，不算失败。不跟随重定向（管理面探针 client）。

use crate::error::UpstreamError;
use crate::http::{HttpPool, Outbound};
use serde_json::Value;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// 缺省探测地址。
pub const DEFAULT_TARGET: &str = "https://www.cloudflare.com/cdn-cgi/trace";
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 8 * 1024;

/// 一次探测的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub status: u16,
    pub latency_ms: u32,
    pub exit_ip: Option<String>,
    pub country: Option<String>,
}

/// 探测失败：`code` 是稳定分类（给前端选文案），`detail` 是错误链摘要（排障用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub code: &'static str,
    pub detail: String,
}

/// 经 `proxy_url`（None = 直连，对照用）请求 `target`。
pub async fn probe(
    http: &HttpPool,
    proxy_url: Option<&str>,
    target: &str,
) -> Result<ProbeResult, ProbeError> {
    let outbound = Outbound {
        proxy_url: proxy_url.map(str::to_owned),
        ..Outbound::default()
    };
    let started = Instant::now();
    let request = http
        .probe(&outbound, reqwest::Method::GET, target)
        .map_err(|error| ProbeError {
            code: "probe_build",
            detail: build_detail(&error),
        })?;
    let mut response = request
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|e| failure(&e))?;
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let status = response.status().as_u16();
    let mut body = Vec::new();
    while body.len() < MAX_BODY
        && let Some(chunk) = response.chunk().await.map_err(|e| failure(&e))?
    {
        let take = chunk.len().min(MAX_BODY - body.len());
        body.extend_from_slice(&chunk[..take]);
    }
    let (exit_ip, country) = parse_exit(&body);
    Ok(ProbeResult {
        status,
        latency_ms,
        exit_ip,
        country,
    })
}

fn build_detail(error: &UpstreamError) -> String {
    match error {
        UpstreamError::Build(reason) | UpstreamError::Connect(reason) => reason.clone(),
        other => other.error_code().to_owned(),
    }
}

fn failure(e: &reqwest::Error) -> ProbeError {
    let code = if e.is_connect() {
        if e.is_timeout() {
            "probe_connect_timeout"
        } else {
            "probe_connect"
        }
    } else if e.is_timeout() {
        "probe_timeout"
    } else {
        "probe_failed"
    };
    ProbeError {
        code,
        detail: error_chain(e),
    }
}

/// 错误链逐层拼起来：reqwest 顶层只说「发送失败」，真正的原因（拒绝连接 / 隧道 407 /
/// 证书不对）在 source 里。截断到 300 字符。
fn error_chain(e: &reqwest::Error) -> String {
    let mut parts = vec![e.to_string()];
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        let text = cause.to_string();
        if parts.last() != Some(&text) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(": ").chars().take(300).collect()
}

/// 从探测响应里认出口 IP 与国家：Cloudflare trace 的 `ip=` / `loc=` 行，常见 JSON
/// （`ip` / `query` / `origin` 与 `country_code` / `countryCode` / `country`），或整个响应就是一个 IP。
/// IP 必须能解析、国家必须是两位字母，认不出就不填。
fn parse_exit(body: &[u8]) -> (Option<String>, Option<String>) {
    let text = String::from_utf8_lossy(body);
    let valid_ip = |s: &str| s.trim().parse::<IpAddr>().ok().map(|ip| ip.to_string());
    let valid_country = |s: &str| {
        let s = s.trim();
        (s.len() == 2 && s.bytes().all(|b| b.is_ascii_alphabetic())).then(|| s.to_ascii_uppercase())
    };
    let mut ip = None;
    let mut country = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ip=") {
            ip = valid_ip(v);
        } else if let Some(v) = line.strip_prefix("loc=") {
            country = valid_country(v);
        }
    }
    if ip.is_some() {
        return (ip, country);
    }
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        let field = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| value.get(*k).and_then(Value::as_str))
                .map(str::to_owned)
        };
        return (
            field(&["ip", "query", "origin"]).and_then(|s| valid_ip(&s)),
            field(&["country_code", "countryCode", "country"]).and_then(|s| valid_country(&s)),
        );
    }
    (valid_ip(&text), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_ip_comes_from_trace_json_or_plain_text_and_is_validated() {
        let trace = b"fl=1\nh=www.cloudflare.com\nip=203.0.113.7\nts=1\nloc=jp\nwarp=off\n";
        assert_eq!(
            parse_exit(trace),
            (Some("203.0.113.7".into()), Some("JP".into()))
        );
        assert_eq!(
            parse_exit(br#"{"ip":"2001:db8::1","country":"US"}"#),
            (Some("2001:db8::1".into()), Some("US".into()))
        );
        assert_eq!(
            parse_exit(br#"{"query":"198.51.100.2","countryCode":"DE","country":"Germany"}"#),
            (Some("198.51.100.2".into()), Some("DE".into()))
        );
        assert_eq!(
            parse_exit(b" 192.0.2.1\n"),
            (Some("192.0.2.1".into()), None)
        );
        // 认不出就不填，不把网页正文当 IP 存
        assert_eq!(parse_exit(b"<html>blocked</html>"), (None, None));
        assert_eq!(parse_exit(b"ip=not-an-ip\nloc=USA"), (None, None));
    }

    #[tokio::test]
    async fn probe_reports_reachability_through_a_forward_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // 一个最小 HTTP 正向代理：收到绝对形式的请求行就直接回 trace 风格的响应
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = "ip=203.0.113.9\nloc=SG\n";
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            head
        });
        let pool = HttpPool::new().unwrap();
        let result = probe(
            &pool,
            Some(&format!("http://{addr}")),
            "http://probe.invalid/cdn-cgi/trace",
        )
        .await
        .unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.exit_ip.as_deref(), Some("203.0.113.9"));
        assert_eq!(result.country.as_deref(), Some("SG"));
        let head = server.await.unwrap();
        assert!(head.starts_with("GET http://probe.invalid/cdn-cgi/trace HTTP/1.1"));
    }

    #[tokio::test]
    async fn dead_proxy_is_a_connect_failure() {
        // 先占一个端口再放掉，拿到一个大概率没人监听的地址
        let addr = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let pool = HttpPool::new().unwrap();
        let error = probe(
            &pool,
            Some(&format!("http://{addr}")),
            "http://probe.invalid/",
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "probe_connect");
    }
}
