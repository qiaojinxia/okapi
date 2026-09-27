//! One HTTP/SSE Responses request for a WS ingress turn. No replay or protocol conversion.
use crate::{ChatEvent, HttpPool, Outbound, StreamHandle, UpstreamError, responses};
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures::StreamExt;

const MAX_BODY: usize = 128 * 1024 * 1024;

pub async fn send(
    http: &HttpPool,
    url: String,
    headers: &[(&str, &str)],
    body: Bytes,
    outbound: &Outbound,
) -> Result<StreamHandle, UpstreamError> {
    // Probe clients retain proxy/TLS settings but do not redirect credentials.
    let mut request = http
        .probe(outbound, reqwest::Method::POST, url)?
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| invalid("responses_http_transport"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after_secs = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| invalid("responses_http_error_body"))?
        {
            let take = chunk.len().min(64 * 1024 - body.len());
            body.extend_from_slice(&chunk[..take]);
            if body.len() == 64 * 1024 {
                break;
            }
        }
        return Err(UpstreamError::Status {
            status,
            body: body.into(),
            retry_after_secs,
        });
    }
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
        })
    {
        return Err(invalid("responses_http_requires_sse"));
    }
    let upstream_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    // Cap bytes before the SSE parser buffers an event. This also bounds the entire turn.
    let chunks = response
        .bytes_stream()
        .scan((0usize, false), |state, item| {
            let output = if state.1 {
                None
            } else {
                match item {
                    Ok(chunk) if state.0.saturating_add(chunk.len()) <= MAX_BODY => {
                        state.0 += chunk.len();
                        Some(Ok(chunk))
                    }
                    _ => {
                        state.1 = true;
                        Some(Err(invalid("responses_http_stream_limit_or_transport")))
                    }
                }
            };
            futures::future::ready(output)
        });
    let events = chunks.eventsource().flat_map(|item| {
        futures::stream::iter(match item {
            Ok(event) => responses::parse_event(&event.event, &event.data)
                .into_iter()
                .map(Ok)
                .collect::<Vec<_>>(),
            Err(_) => vec![Err(invalid("responses_http_stream"))],
        })
    });
    Ok(StreamHandle {
        upstream_request_id,
        events: Box::pin(events),
    })
}

fn invalid(reason: &'static str) -> UpstreamError {
    UpstreamError::Session {
        reason,
        timed_out: false,
    }
}

/// Attach the ingress lane after parsing; HTTP upstreams must not select a different lane.
pub fn lane(mut event: ChatEvent, lane: Option<&str>) -> Result<ChatEvent, UpstreamError> {
    if let ChatEvent::Data {
        raw, event: name, ..
    } = &mut event
    {
        let mut value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| invalid("responses_http_invalid_event"))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| invalid("responses_http_invalid_event"))?;
        if object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            let name = name
                .as_deref()
                .filter(|n| !n.is_empty())
                .ok_or_else(|| invalid("responses_http_invalid_event"))?;
            object.insert("type".into(), name.into());
        }
        if let Some(upstream_lane) = object.get("stream_id")
            && upstream_lane.as_str() != lane
        {
            return Err(invalid("responses_http_wrong_stream"));
        }
        if let Some(lane) = lane {
            object.insert("stream_id".into(), lane.into());
        }
        *raw = value.to_string();
    }
    Ok(event)
}
