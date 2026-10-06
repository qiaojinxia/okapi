use bytes::Bytes;
use okapi_providers::convert::{
    anthropic_to_openai::OaiStreamToAnthropic,
    gemini_to_openai::OaiStreamToGemini,
    responses_to_chat::{ChatStreamToResponses, response_chat_to_responses},
};
use okapi_providers::{ChatEvent, UpstreamError};
use serde_json::{Value, json};
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)] // Protocol fixtures accept owned JSON and feed fallible stream steps.
fn data(value: Value) -> Result<ChatEvent, UpstreamError> {
    Ok(ChatEvent::Data {
        raw: value.to_string(),
        event: None,
        has_output: true,
        content_chars: 0,
        usage: None,
    })
}
#[test]
fn parallel_tool_arguments_keep_distinct_anthropic_blocks() {
    let mut converter = OaiStreamToAnthropic::new("fixture");
    let chunks = [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"first","arguments":"{"}},{"index":1,"id":"call_b","function":{"name":"second","arguments":"{"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"x\":1}"}},{"index":1,"function":{"arguments":"\"x\":2}"}}]}}]}),
    ];
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(converter.step(data(chunk)));
    }
    events.extend(converter.step(Ok(ChatEvent::Done)));
    let mut blocks = std::collections::BTreeMap::new();
    let mut stopped = std::collections::HashSet::new();
    for event in events {
        if let ChatEvent::Data {
            raw,
            event: Some(kind),
            ..
        } = event.unwrap()
        {
            let value: Value = serde_json::from_str(&raw).unwrap();
            let index = value["index"].as_u64().unwrap_or(0);
            match kind.as_str() {
                "content_block_start" => {
                    blocks.insert(
                        index,
                        (
                            value["content_block"]["id"].as_str().unwrap().to_owned(),
                            String::new(),
                        ),
                    );
                }
                "content_block_delta" => {
                    if let Some(part) = value["delta"]["partial_json"].as_str() {
                        blocks.get_mut(&index).unwrap().1.push_str(part);
                    }
                }
                "content_block_stop" => {
                    stopped.insert(index);
                }
                _ => {}
            }
        }
    }
    assert_eq!(blocks.len(), 2);
    for (index, (id, arguments)) in blocks {
        assert!(stopped.contains(&index));
        let arguments: Value = serde_json::from_str(&arguments).unwrap();
        assert_eq!(arguments, json!({"x":if id == "call_a" {1} else {2}}));
    }
}
#[test]
fn responses_preserve_length_and_filter_termination_in_json_and_sse() {
    for (finish, reason) in [
        ("length", "max_output_tokens"),
        ("content_filter", "content_filter"),
    ] {
        let response=Bytes::from(json!({"id":"fixture","model":"fixture","choices":[{"message":{"content":"cut"},"finish_reason":finish}]}).to_string());
        let (body, _) = response_chat_to_responses(&response).unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["status"], "incomplete");
        assert_eq!(value["incomplete_details"]["reason"], reason);
        let mut converter = ChatStreamToResponses::new("fixture");
        converter.step(data(
            json!({"choices":[{"delta":{"content":"cut"},"finish_reason":finish}]}),
        ));
        let events = converter.step(Ok(ChatEvent::Done));
        let values: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event.unwrap() {
                ChatEvent::Data {
                    raw,
                    event: Some(kind),
                    ..
                } => Some((kind, serde_json::from_str::<Value>(&raw).unwrap())),
                _ => None,
            })
            .collect();
        let kinds: Vec<_> = values.iter().map(|(kind, _)| kind.as_str()).collect();
        for expected in [
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.incomplete",
        ] {
            assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
        }
        assert!(!kinds.contains(&"response.completed"));
        assert_eq!(
            values
                .iter()
                .find(|(kind, _)| kind == "response.incomplete")
                .unwrap()
                .1["response"]["incomplete_details"]["reason"],
            reason
        );
    }
}
#[test]
fn hostile_tool_indices_fail_before_unbounded_growth() {
    for index in [128, 262_143, i64::MAX] {
        let chunk = json!({"choices":[{"delta":{"tool_calls":[{"index":index,"id":"call_a","function":{"name":"first","arguments":"{}"}}]}}]});
        let mut converter = OaiStreamToGemini::new("fixture");
        assert!(
            converter
                .step(data(chunk.clone()))
                .iter()
                .any(Result::is_err)
        );
        let mut converter = OaiStreamToAnthropic::new("fixture");
        assert!(converter.step(data(chunk)).iter().any(Result::is_err));
    }
}
#[tokio::test]
async fn bounded_collector_propagates_middle_stream_errors() {
    let stream = futures::stream::iter([
        Ok(Bytes::from_static(b"prefix")),
        Err(UpstreamError::Stream("truncated".into())),
        Ok(Bytes::from_static(b"suffix")),
    ]);
    assert!(okapi_providers::limits::collect(stream, 64).await.is_err());
    let stream = futures::stream::iter([Ok(Bytes::from_static(b"oversized"))]);
    assert!(okapi_providers::limits::collect(stream, 2).await.is_err());
}
async fn raw_upstream(status: u16, extra: &str, body: Vec<u8>, truncate: bool) -> String {
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
        let length = body.len() + if truncate { 50 } else { 0 };
        let header = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {length}\r\nConnection: close\r\n{extra}\r\n"
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    format!("http://{address}")
}
#[tokio::test]
async fn status_and_retry_after_survive_a_truncated_error_body() {
    let outbound = okapi_providers::Outbound::default();
    let upstream = okapi_providers::OpenAiUpstream::new().unwrap();
    let base = raw_upstream(
        429,
        "Retry-After: 3600\r\nContent-Type: application/json\r\n",
        b"broken".to_vec(),
        true,
    )
    .await;
    let Err(error) = upstream
        .speech(&base, "synthetic", Bytes::from_static(b"{}"), &outbound)
        .await
    else {
        panic!("429 expected")
    };
    assert_eq!(error.upstream_status(), Some(429));
    assert_eq!(error.retry_after_secs(), Some(3600));
}
#[tokio::test]
async fn count_tokens_preserves_limit_headers_even_when_error_body_is_truncated() {
    let outbound = okapi_providers::Outbound::default();
    let upstream = okapi_providers::AnthropicUpstream::new().unwrap();
    for subscription in [false, true] {
        for truncated in [false, true] {
            let base = raw_upstream(
                429,
                "Retry-After: 1800\r\nContent-Type: application/json\r\n",
                br#"{"error":{"message":"rate limit"}}"#.to_vec(),
                truncated,
            )
            .await;
            let body = Bytes::from_static(br#"{"model":"fixture","messages":[]}"#);
            let result = if subscription {
                okapi_providers::oauth::anthropic_max::count_tokens(
                    upstream.http(),
                    &base,
                    "synthetic",
                    body,
                    &outbound,
                    None,
                )
                .await
            } else {
                upstream
                    .count_tokens(&base, "synthetic", body, &outbound)
                    .await
            };
            let error = result.expect_err("429 must remain an upstream status error");
            assert_eq!(error.upstream_status(), Some(429));
            assert_eq!(error.retry_after_secs(), Some(1800));
        }
    }
}
#[tokio::test]
async fn bedrock_partial_frame_at_eof_is_an_error() {
    use futures::StreamExt;
    let first=okapi_providers::aws_eventstream::encode_frame(&[(":message-type","event"),(":event-type","chunk")],br#"{"bytes":"eyJ0eXBlIjoiY29udGVudF9ibG9ja19kZWx0YSIsImluZGV4IjowLCJkZWx0YSI6eyJ0eXBlIjoidGV4dF9kZWx0YSIsInRleHQiOiJoaSJ9fQ=="}"#);
    let last = okapi_providers::aws_eventstream::encode_frame(
        &[(":message-type", "event"), (":event-type", "chunk")],
        br#"{"bytes":"eyJ0eXBlIjoibWVzc2FnZV9zdG9wIn0="}"#,
    );
    let mut body = first;
    body.extend_from_slice(&last[..last.len() / 2]);
    let base = raw_upstream(
        200,
        "Content-Type: application/vnd.amazon.eventstream\r\n",
        body,
        false,
    )
    .await;
    let response = okapi_providers::BedrockUpstream::new()
        .unwrap()
        .messages(
            &base,
            Some("us-east-1"),
            "synthetic",
            "claude",
            Bytes::from_static(br#"{"messages":[]}"#),
            true,
            &okapi_providers::Outbound::default(),
        )
        .await
        .unwrap();
    let okapi_providers::anthropic::MessagesResponse::Stream(handle) = response else {
        panic!("stream expected")
    };
    let events: Vec<_> = handle.events.collect().await;
    assert!(
        events.iter().any(Result::is_err),
        "EOF cannot silently finish a partial frame"
    );
}
#[tokio::test]
async fn new_inference_transport_requires_only_a_registration() {
    use okapi_providers::{
        inference::{self, Request, Response, Transport},
        registry,
    };
    struct Fixture;
    impl Transport for Fixture {
        fn infer<'a>(
            &'a self,
            request: Request<'a>,
        ) -> futures::future::BoxFuture<'a, Result<Response, UpstreamError>> {
            Box::pin(async move {
                assert_eq!(request.credential, "synthetic");
                Ok(Response::OpenAi(okapi_providers::ChatResponse::Json {
                    body: request.body,
                    usage: None,
                    upstream_request_id: None,
                    status: 200,
                }))
            })
        }
    }
    fn factory(_: &inference::Resources) -> std::sync::Arc<dyn Transport> {
        std::sync::Arc::new(Fixture)
    }
    let registration = registry::ProviderDescriptor {
        id: "fixture-extension",
        inference: Some(factory),
        ..registry::BUILT_INS[0]
    };
    let resources = inference::Resources::new().unwrap();
    let plugins = inference::Registry::from_registrations(&resources, &[registration]).unwrap();
    let response = plugins
        .infer(
            "fixture-extension",
            Request {
                surface: inference::Surface::Chat,
                base: "https://unused.test",
                model: "fixture",
                credential: "synthetic",
                account_id: None,
                region: None,
                api_version: None,
                body: Bytes::from_static(b"{}"),
                stream: false,
                outbound: &okapi_providers::Outbound::default(),
            },
        )
        .await
        .unwrap();
    let Response::OpenAi(okapi_providers::ChatResponse::Json { body, .. }) = response else {
        panic!("wrong registered transport")
    };
    assert_eq!(body, Bytes::from_static(b"{}"));
}
