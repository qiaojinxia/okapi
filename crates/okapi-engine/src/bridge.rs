//! Control-plane to data-plane bridge trait.
//!
//! `okapi-console` uses this thin interface to relay Playground (`/api/me/playground/*`)
//! and channel try-out (`/admin/channels/{id}/try`) requests directly into the in-process
//! data-plane handlers without depending on `okapi-gateway` at compile time.

use crate::state::AppState;
use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::response::Response;
use okapi_kernel::error::AppError;
use okapi_providers::model_parameters::ParameterProfile;

/// In-process data-plane entrypoints invoked by control-plane relay endpoints.
#[async_trait::async_trait]
pub trait DataPlaneBridge: Send + Sync {
    async fn chat_completions(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response;

    async fn responses(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response;

    async fn playground_parameters(
        &self,
        state: &AppState,
        headers: &HeaderMap,
        model: &str,
    ) -> Result<ParameterProfile, AppError>;
}
