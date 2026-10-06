//! Bounded control-plane response collection; stream failures never look like EOF.
use crate::gateway::error::AppError;
use axum::http::StatusCode;
use bytes::Bytes;
use futures::Stream;
use okapi_providers::UpstreamError;

pub(super) async fn collect(
    stream: impl Stream<Item = Result<Bytes, UpstreamError>> + Unpin,
    maximum: usize,
) -> Result<Bytes, AppError> {
    okapi_providers::limits::collect(stream, maximum)
        .await
        .map_err(|error| {
            AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                .with_param(error.error_code())
        })
}
