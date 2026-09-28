//! Durable, tenant-owned image work. Execution lives in `worker`, never in the HTTP request.
mod execute;
pub mod objects;
pub use execute::{run_one, run_worker};

use super::{AppError, AppState, request::Input};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use okapi_api::codes;
use okapi_store::image_tasks::{self as store, Artifact, Enqueued, NewTask, Task};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub(super) struct Lease {
    pub id: Uuid,
    pub token: Uuid,
}

pub async fn generations(State(state): State<AppState>, req: Request) -> Response {
    submit(state, req, false).await
}
pub async fn edits(State(state): State<AppState>, req: Request) -> Response {
    submit(state, req, true).await
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn task_id(id: Uuid) -> String {
    format!("imgtask_{}", id.simple())
}
fn poll_url(id: Uuid) -> String {
    format!("/v1/images/tasks/{}", task_id(id))
}
fn parse_id(value: &str) -> Result<Uuid, AppError> {
    value
        .strip_prefix("imgtask_")
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(not_found)
}
fn not_found() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
}

pub(super) async fn enabled(state: &AppState) -> bool {
    state
        .setting_cached("image_tasks_enabled")
        .await
        .as_ref()
        .as_ref()
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn public(task: &Task) -> Value {
    let mut body = json!({
        "id":task_id(task.id),"task_id":task_id(task.id),"object":"image.generation.task",
        "status":if task.status=="preparing" {"processing"}else{&task.status},
        "model":task.model_name,"created_at":task.created_at.timestamp(),
        "expires_at":task.expires_at.timestamp(),"poll_url":poll_url(task.id),
        "cancel_requested":task.cancel_requested,
    });
    if let Some(id) = task.reservation_id {
        body["request_id"] = id.to_string().into();
    }
    if let Some(at) = task.completed_at {
        body["completed_at"] = at.timestamp().into();
    }
    if let Some(status) = task.http_status {
        body["http_status"] = status.into();
    }
    if let Some(result) = &task.result {
        body["result"] = result.clone();
        if let Some(url) = result.pointer("/data/0/url") {
            body["image_url"] = url.clone();
        }
    }
    if let Some(error) = &task.error {
        body["error"] = error.clone();
    }
    body
}

fn response(task: &Task, status: StatusCode) -> Response {
    let mut response = (status, axum::Json(public(task))).into_response();
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
        .headers_mut()
        .insert("retry-after", axum::http::HeaderValue::from_static("3"));
    if let Ok(location) = poll_url(task.id).parse() {
        response.headers_mut().insert("location", location);
    }
    super::super::error::with_request_id(response, task.id)
}

async fn submit(state: AppState, req: Request, edit: bool) -> Response {
    let request_id = Uuid::new_v4();
    let result = async {
        let headers = req.headers().clone();
        let key = super::super::auth::authenticate_data_plane(&state, &headers).await?;
        if !enabled(&state).await {
            return Err(not_found());
        }
        let input = super::request::read(req, &state, edit).await?;
        input.require_nonstream()?;
        // Validate access/pricing now; execution rechecks current permissions and prices.
        super::prepare(&state, &key, &input).await?;
        let payload = input.encode()?;
        if payload.len() > store::MAX_PAYLOAD_BYTES {
            return Err(
                AppError::new(StatusCode::PAYLOAD_TOO_LARGE, codes::BAD_REQUEST).with_param("body"),
            );
        }
        let idempotency = headers
            .get("idempotency-key")
            .map(|header| {
                let value = header
                    .to_str()
                    .map_err(|_| AppError::bad_request().with_param("idempotency_key"))?;
                if value.is_empty()
                    || value.len() > 128
                    || !value.bytes().all(|b| b.is_ascii_graphic())
                {
                    return Err(AppError::bad_request().with_param("idempotency_key"));
                }
                Ok(hash(value.as_bytes()))
            })
            .transpose()?;
        let ip = super::super::clients::client_ip(&headers).map(|ip| ip.to_string());
        let queued = store::enqueue(
            &state.pg,
            NewTask {
                id: request_id,
                user_id: key.user_id,
                api_key_id: key.key_id,
                kind: if edit { "edit" } else { "generation" },
                model: &input.model,
                request_hash: &hash(&payload),
                idempotency_hash: idempotency.as_deref(),
                payload: &payload,
                client_ip: ip.as_deref(),
                client_type: super::super::clients::detect_client_type(&headers),
            },
            store::Limits::default(),
        )
        .await?;
        match queued {
            Enqueued::Created(task) | Enqueued::Existing(task) => {
                Ok(response(&task, StatusCode::ACCEPTED))
            }
            Enqueued::Conflict => Err(AppError::new(StatusCode::CONFLICT, codes::BAD_REQUEST)
                .with_param("idempotency_key_conflict")),
            Enqueued::Capacity => Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                codes::RATE_LIMITED,
            )
            .with_param("image_task_capacity")),
        }
    }
    .await;
    result.unwrap_or_else(|error: AppError| error.into_response_with(Some(request_id)))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    owned(state, id, headers, false).await
}
pub async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    owned(state, id, headers, true).await
}
async fn owned(state: AppState, id: String, headers: HeaderMap, cancel: bool) -> Response {
    let request_id = Uuid::new_v4();
    let result = async {
        let key = super::super::auth::authenticate_data_plane(&state, &headers).await?;
        let id = parse_id(&id)?;
        let task = if cancel {
            store::cancel(&state.pg, id, key.user_id, key.key_id).await?
        } else {
            store::get_owned(&state.pg, id, key.user_id, key.key_id).await?
        }
        .ok_or_else(not_found)?;
        Ok(response(&task, StatusCode::OK))
    }
    .await;
    result.unwrap_or_else(|error: AppError| error.into_response_with(Some(request_id)))
}

pub async fn content(
    State(state): State<AppState>,
    Path((id, index)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let request_id = Uuid::new_v4();
    let result = async {
        let key = super::super::auth::authenticate_data_plane(&state, &headers).await?;
        let id = parse_id(&id)?;
        let index = index
            .parse::<i32>()
            .ok()
            .filter(|v| (0..10).contains(v))
            .ok_or_else(not_found)?;
        let permit = state
            .image_download_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                    .with_param("image_download_capacity")
            })?;
        let artifact = store::artifact_owned(&state.pg, id, index, key.user_id, key.key_id)
            .await?
            .ok_or_else(not_found)?;
        let (bytes, mime) = state
            .image_storage
            .download(artifact)
            .await
            .map_err(objects::error)?;
        let length = bytes.len();
        let stream = futures::stream::unfold(
            (Bytes::from(bytes), permit),
            |(mut bytes, permit)| async move {
                if bytes.is_empty() {
                    return None;
                }
                let chunk = bytes.split_to(bytes.len().min(64 * 1024));
                Some((Ok::<_, std::convert::Infallible>(chunk), (bytes, permit)))
            },
        );
        Response::builder()
            .header("content-type", mime)
            .header("content-length", length)
            .header("cache-control", "private, no-store")
            .header("x-content-type-options", "nosniff")
            .header("content-disposition", "attachment")
            .body(Body::from_stream(stream))
            .map(|r| super::super::error::with_request_id(r, request_id))
            .map_err(|_| AppError::internal())
    }
    .await;
    result.unwrap_or_else(|error: AppError| error.into_response_with(Some(request_id)))
}

async fn artifacts(
    state: &AppState,
    id: Uuid,
    body: &[u8],
) -> Result<(Value, Vec<Artifact>), AppError> {
    let invalid = || {
        AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
            .with_param("invalid_image_response")
    };
    let mut value: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    let data = value
        .get_mut("data")
        .and_then(Value::as_array_mut)
        .ok_or_else(invalid)?;
    let mut artifacts = Vec::new();
    let mut remaining = usize::try_from(store::RESULT_BUDGET_BYTES).map_err(|_| invalid())?;
    for (index, image) in data.iter_mut().enumerate() {
        let content = if let Some(base64) = image.get("b64_json").and_then(Value::as_str) {
            Some(
                base64::prelude::BASE64_STANDARD
                    .decode(base64)
                    .map_err(|_| invalid())?,
            )
        } else if state.image_storage.copy_urls {
            let url = image
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            Some(
                okapi_providers::image_store::fetch::image(
                    url,
                    &state.image_storage.fetch,
                    remaining,
                )
                .await
                .map_err(objects::error)?
                .0,
            )
        } else {
            None
        };
        if let Some(content) = content {
            if content.is_empty() {
                return Err(invalid());
            }
            remaining = remaining.checked_sub(content.len()).ok_or_else(invalid)?;
            let mime = okapi_providers::image_store::fetch::content_type(&content)
                .unwrap_or("application/octet-stream");
            artifacts.push(Artifact {
                index: i32::try_from(index).map_err(|_| invalid())?,
                content,
                content_type: mime.into(),
            });
            image
                .as_object_mut()
                .ok_or_else(invalid)?
                .remove("b64_json");
            image["url"] = format!("{}/content/{index}", poll_url(id)).into();
        }
    }
    Ok((value, artifacts))
}

pub(super) async fn complete(
    state: &AppState,
    lease: Lease,
    body: &[u8],
    input: okapi_ledger::SettlementInput<'_>,
) -> Result<(), AppError> {
    use sqlx::Connection as _;
    let (result, artifacts) = artifacts(state, lease.id, body).await?;
    let mut guard = okapi_ledger::holds::UserGuard::acquire(&state.pg, input.user_id).await?;
    let mut tx = guard
        .connection()
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    if !store::lock_live(&mut tx, lease.id, lease.token).await? {
        return Err(AppError::internal().with_param("image_task_lease_lost"));
    }
    okapi_ledger::sync::record_in_tx(&mut tx, input.clone()).await?;
    store::complete(
        &mut tx,
        lease.id,
        &result,
        i32::from(input.upstream_status.unwrap_or(200)),
        &artifacts,
    )
    .await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    if let Err(error) = guard.synchronize(&state.ledger).await {
        tracing::error!(request_id=%input.request_id,%error,"image bill awaiting Redis recovery");
    }
    Ok(())
}
