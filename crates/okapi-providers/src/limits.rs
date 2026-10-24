//! Bounds apply before SSE parsing, including fragmented lines and multiline events.
use crate::UpstreamError;
use bytes::Bytes;
use futures::{Stream, StreamExt};

const MAX_EVENT: usize = 16 * 1024 * 1024;
pub(crate) const MAX_STREAM: usize = 64 * 1024 * 1024;
pub(crate) const MAX_BODY: usize = 64 * 1024 * 1024;
pub(crate) const MAX_ERROR: usize = 1024 * 1024;

/// Shared bounded collector for control-plane and inference response streams.
pub async fn collect(
    mut stream: impl Stream<Item = Result<Bytes, UpstreamError>> + Unpin,
    maximum: usize,
) -> Result<Bytes, UpstreamError> {
    tokio::time::timeout(std::time::Duration::from_mins(2), async {
        let mut bytes = bytes::BytesMut::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if chunk.len() > maximum.saturating_sub(bytes.len()) {
                return Err(UpstreamError::Build("upstream_response_too_large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes.freeze())
    })
    .await
    .map_err(|_| UpstreamError::Timeout)?
}

/// 流式响应的累计字节记账：加上这一块之后超过 `limit` 就报错，不超过就记上。
pub(crate) fn charge(total: &mut usize, chunk: usize, limit: usize) -> Result<(), UpstreamError> {
    if chunk > limit.saturating_sub(*total) {
        return Err(UpstreamError::Stream("upstream_stream_too_large".into()));
    }
    *total += chunk;
    Ok(())
}

pub(crate) fn sse(
    response: reqwest::Response,
) -> impl Stream<Item = Result<Bytes, UpstreamError>> + Send {
    bounded(
        response
            .bytes_stream()
            .map(|c| c.map_err(|_| UpstreamError::Stream("upstream_read".into()))),
        MAX_EVENT,
        MAX_STREAM,
    )
}

fn bounded(
    source: impl Stream<Item = Result<Bytes, UpstreamError>> + Send + 'static,
    event_limit: usize,
    total_limit: usize,
) -> impl Stream<Item = Result<Bytes, UpstreamError>> + Send {
    futures::stream::try_unfold(
        (Box::pin(source), 0usize, 0usize, 0usize, false),
        move |(mut source, mut total, mut event, mut line, mut cr)| async move {
            let Some(chunk) = source.next().await else {
                return Ok(None);
            };
            let chunk = chunk?;
            charge(&mut total, chunk.len(), total_limit)?;
            for &byte in &chunk {
                if byte == b'\n' && cr {
                    cr = false;
                    continue;
                }
                cr = byte == b'\r';
                event = event.saturating_add(1);
                if event > event_limit {
                    return Err(UpstreamError::Stream("upstream_event_too_large".into()));
                }
                if matches!(byte, b'\r' | b'\n') {
                    if line == 0 {
                        event = 0;
                    }
                    line = 0;
                } else {
                    line = line.saturating_add(1);
                }
            }
            Ok(Some((chunk, (source, total, event, line, cr))))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn charge_accumulates_and_refuses_the_chunk_that_crosses_the_limit() {
        let mut total = 0;
        assert!(charge(&mut total, 6, 10).is_ok());
        assert!(charge(&mut total, 4, 10).is_ok(), "刚好到上限还可以");
        assert_eq!(total, 10);
        assert!(matches!(
            charge(&mut total, 1, 10),
            Err(UpstreamError::Stream(reason)) if reason == "upstream_stream_too_large"
        ));
        assert_eq!(total, 10, "被拒的一块不记账");
        let mut near_overflow = usize::MAX - 1;
        assert!(charge(&mut near_overflow, 2, usize::MAX).is_err());
    }

    #[tokio::test]
    async fn fragmented_and_multiline_limits() {
        let input = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"data:12\r")),
            Ok(Bytes::from_static(b"\ndata:34\r\n\r\n")),
        ]);
        assert!(
            bounded(input, 12, 100)
                .collect::<Vec<_>>()
                .await
                .iter()
                .any(Result::is_err)
        );
        let input =
            futures::stream::iter(vec![Ok(Bytes::from_static(b"data:1\r\n\r\ndata:2\n\n"))]);
        assert!(
            bounded(input, 12, 100)
                .collect::<Vec<_>>()
                .await
                .iter()
                .all(Result::is_ok)
        );
    }
}
