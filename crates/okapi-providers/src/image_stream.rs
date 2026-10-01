//! Bounded Images SSE transport, shared by OpenAI-compatible and Azure endpoints.
use crate::openai::{EmbeddingsResponse, MAX_IMAGE_RESPONSE_BYTES, classify, response_bytes};
use crate::{AzureUpstream, OpenAiUpstream, Outbound, UpstreamError};
use bytes::Bytes;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use std::{pin::Pin, time::Duration};

/// Shorter than the ordinary ledger reservation lease (10 minutes).
pub const IMAGE_STREAM_TIMEOUT: Duration = Duration::from_mins(7);

pub enum ImageBody {
    Json(Bytes),
    Multipart(Vec<(String, Option<String>, Option<String>, Bytes)>),
}

pub struct ImageEvent {
    pub event: String,
    pub data: String,
}

pub struct ImageStream {
    pub status: u16,
    pub upstream_request_id: Option<String>,
    pub events: Pin<Box<dyn Stream<Item = Result<ImageEvent, UpstreamError>> + Send>>,
}

pub enum ImageResponse {
    Stream(ImageStream),
    /// Some compatible upstreams ignore stream=true and return a single JSON response.
    Json(EmbeddingsResponse),
}

impl OpenAiUpstream {
    pub async fn image_stream(
        &self,
        base: &str,
        path: &str,
        credential: &str,
        body: ImageBody,
        outbound: &Outbound,
    ) -> Result<ImageResponse, UpstreamError> {
        let req = self.http.probe(
            outbound,
            reqwest::Method::POST,
            format!("{}{path}", base.trim_end_matches('/')),
        )?;
        send(req.bearer_auth(credential), body).await
    }
}

impl AzureUpstream {
    #[allow(clippy::too_many_arguments)]
    pub async fn image_stream(
        &self,
        endpoint: &str,
        version: &str,
        deployment: &str,
        path: &str,
        credential: &str,
        body: ImageBody,
        outbound: &Outbound,
    ) -> Result<ImageResponse, UpstreamError> {
        send(
            self.post(endpoint, deployment, path, version, credential, outbound)?,
            body,
        )
        .await
    }
}

fn request_body(
    req: reqwest::RequestBuilder,
    body: ImageBody,
) -> Result<reqwest::RequestBuilder, UpstreamError> {
    Ok(match body {
        ImageBody::Json(bytes) => req.header("content-type", "application/json").body(bytes),
        ImageBody::Multipart(parts) => {
            let mut form = reqwest::multipart::Form::new();
            for (name, file, mime, bytes) in parts {
                let mut part = reqwest::multipart::Part::bytes(bytes.to_vec());
                if let Some(file) = file {
                    part = part.file_name(file);
                }
                if let Some(mime) = mime {
                    part = part
                        .mime_str(&mime)
                        .map_err(|_| UpstreamError::Build("image_mime".into()))?;
                }
                form = form.part(name, part);
            }
            req.multipart(form)
        }
    })
}

async fn send(
    req: reqwest::RequestBuilder,
    body: ImageBody,
) -> Result<ImageResponse, UpstreamError> {
    let response = request_body(req, body)?
        .header("accept", "text/event-stream, application/json")
        .timeout(IMAGE_STREAM_TIMEOUT)
        .send()
        .await
        .map_err(|error| classify(&error))?;
    let status = response.status().as_u16();
    let upstream_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if !(200..300).contains(&status) {
        let retry_after_secs = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok());
        return Err(UpstreamError::Status {
            status,
            body: response_bytes(response, Some(64 * 1024))
                .await
                .unwrap_or_default(),
            retry_after_secs,
        });
    }
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap_or("")
        .trim();
    if mime.eq_ignore_ascii_case("application/json") {
        return Ok(ImageResponse::Json(EmbeddingsResponse {
            status,
            upstream_request_id,
            body: response_bytes(response, Some(MAX_IMAGE_RESPONSE_BYTES)).await?,
            usage: None,
        }));
    }
    if !mime.eq_ignore_ascii_case("text/event-stream") {
        return Err(UpstreamError::Stream("image_content_type".into()));
    }
    let bounded = bounded_lines(
        response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| classify(&error))),
    );
    let events = bounded.eventsource().map(|event| {
        event
            .map(|event| ImageEvent {
                event: event.event,
                data: event.data,
            })
            .map_err(|_| UpstreamError::Stream("image_stream_read".into()))
    });
    Ok(ImageResponse::Stream(ImageStream {
        status,
        upstream_request_id,
        events: Box::pin(events),
    }))
}

/// Accumulate an unfinished line once, not inside the event parser on every network chunk.
/// Only inspect the NEW bytes for delimiters, so a long base64 line is linear in input size.
/// The raw-byte limit applies before allocation and before the parser sees any fragment.
fn bounded_lines(
    source: impl Stream<Item = Result<Bytes, UpstreamError>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, UpstreamError>> + Send {
    futures::stream::try_unfold(
        (Box::pin(source), 0usize, bytes::BytesMut::new()),
        |(mut source, mut used, mut pending)| async move {
            loop {
                let Some(chunk) = source.next().await else {
                    return if pending.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some((pending.split().freeze(), (source, used, pending))))
                    };
                };
                let chunk = chunk?;
                if chunk.len() > MAX_IMAGE_RESPONSE_BYTES.saturating_sub(used) {
                    return Err(UpstreamError::Stream("image_response_too_large".into()));
                }
                used += chunk.len();
                let complete = chunk
                    .iter()
                    .rposition(|byte| matches!(*byte, b'\r' | b'\n'))
                    .map(|index| pending.len() + index + 1);
                pending.extend_from_slice(&chunk);
                if let Some(end) = complete {
                    return Ok(Some((
                        pending.split_to(end).freeze(),
                        (source, used, pending),
                    )));
                }
            }
        },
    )
}
