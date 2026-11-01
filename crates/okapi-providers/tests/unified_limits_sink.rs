//! 订阅限额头的上报走真实传输：成功、失败、流式都在拿到响应头时报给观测钩子，
//! 且不改变原有的错误语义（状态码、Retry-After）。
use bytes::Bytes;
use okapi_providers::unified_limits::{ResponseMeta, Sink, observe_responses};
use std::sync::{Arc, Mutex};

async fn raw_upstream(status: u16, extra: &str, body: Vec<u8>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let extra = extra.to_owned();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..end]);
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let header = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
            body.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    format!("http://{address}")
}

type Seen = Arc<Mutex<Vec<(u16, Option<f64>, Option<String>)>>>;

fn recorder() -> (Sink, Seen) {
    let seen: Seen = Arc::default();
    let sink_seen = Arc::clone(&seen);
    let sink: Sink = Arc::new(move |meta: &ResponseMeta<'_>| {
        let five = meta.limits.window(18_000);
        sink_seen.lock().unwrap().push((
            meta.status,
            five.and_then(|w| w.utilization),
            meta.limits.representative_claim.clone(),
        ));
    });
    (sink, seen)
}

const LIMIT_HEADERS: &str = "anthropic-ratelimit-unified-status: allowed\r\n\
anthropic-ratelimit-unified-representative-claim: five_hour\r\n\
anthropic-ratelimit-unified-5h-utilization: 0.5\r\n\
anthropic-ratelimit-unified-5h-reset: 1793512800\r\n\
anthropic-ratelimit-unified-7d-utilization: 0.1\r\n";

fn request() -> Bytes {
    Bytes::from_static(
        br#"{"model":"fixture","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#,
    )
}

#[tokio::test]
async fn success_failure_and_stream_responses_all_report_their_limit_headers() {
    let upstream = okapi_providers::AnthropicUpstream::new().unwrap();
    let outbound = okapi_providers::Outbound::default();
    let (sink, seen) = recorder();

    let ok = raw_upstream(
        200,
        &format!("Content-Type: application/json\r\n{LIMIT_HEADERS}"),
        br#"{"id":"msg_1","type":"message","content":[],"usage":{}}"#.to_vec(),
    )
    .await;
    let rejected = raw_upstream(
        429,
        &format!("Content-Type: application/json\r\nRetry-After: 1800\r\n{LIMIT_HEADERS}"),
        br#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#.to_vec(),
    )
    .await;
    let streaming = raw_upstream(
        200,
        &format!("Content-Type: text/event-stream\r\n{LIMIT_HEADERS}"),
        b"event: message_stop\ndata: {}\n\n".to_vec(),
    )
    .await;

    observe_responses(sink, async {
        upstream
            .messages(&ok, "synthetic", request(), false, &outbound)
            .await
            .expect("200");
        let error = upstream
            .messages(&rejected, "synthetic", request(), false, &outbound)
            .await
            .err()
            .expect("429");
        assert_eq!(error.upstream_status(), Some(429));
        assert_eq!(
            error.retry_after_secs(),
            Some(1800),
            "上报限额头不改变 Retry-After 语义"
        );
        // 流式：钩子在拿到响应头时就触发，不必等事件读完
        let _stream = upstream
            .messages(&streaming, "synthetic", request(), true, &outbound)
            .await
            .expect("stream");
        assert_eq!(seen.lock().unwrap().len(), 3);
    })
    .await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            (200, Some(0.5), Some("five_hour".to_owned())),
            (429, Some(0.5), Some("five_hour".to_owned())),
            (200, Some(0.5), Some("five_hour".to_owned())),
        ]
    );
}

#[tokio::test]
async fn responses_without_subscription_headers_report_nothing() {
    let upstream = okapi_providers::AnthropicUpstream::new().unwrap();
    let outbound = okapi_providers::Outbound::default();
    let (sink, seen) = recorder();
    let base = raw_upstream(
        200,
        "Content-Type: application/json\r\nanthropic-ratelimit-requests-remaining: 99\r\n",
        br#"{"id":"msg_1","type":"message","content":[],"usage":{}}"#.to_vec(),
    )
    .await;
    observe_responses(sink, async {
        upstream
            .messages(&base, "synthetic", request(), false, &outbound)
            .await
            .expect("200");
    })
    .await;
    assert!(seen.lock().unwrap().is_empty());
}
