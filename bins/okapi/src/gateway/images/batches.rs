//! Native Gemini/Vertex image jobs, with fixed accounts, prices and private results.
mod archive;
mod binding;
pub use archive::download;
mod execute;
mod rate;
mod request;
mod results;
mod view;
pub use execute::{run_cleanup, run_one, run_statistics, run_worker};
pub use view::{cancel, content, delete, get, items, list, models};

use super::{AppError, AppState};
use axum::{
    body::Bytes,
    extract::{FromRequest, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use okapi_api::codes;
use okapi_domain::Money;
use okapi_store::image_batches::{self as store, Batch, UnitQuote};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn public_id(id: Uuid) -> String {
    format!("imgbatch_{}", id.simple())
}
fn parse_id(raw: &str) -> Result<Uuid, AppError> {
    raw.strip_prefix("imgbatch_")
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(not_found)
}
fn not_found() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
}
fn map_store(error: store::Error) -> AppError {
    match error {
        store::Error::Store(e) => e.into(),
        store::Error::Invalid(p) => AppError::bad_request().with_param(p),
        store::Error::Capacity => AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
            .with_param("batch_capacity"),
        store::Error::RateLimited(axis) => {
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(axis)
        }
        store::Error::AdmissionUnavailable => {
            AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::OVERLOADED)
        }
        store::Error::Budget => {
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::INSUFFICIENT_QUOTA)
                .with_param("batch_budget")
        }
        store::Error::IdempotencyConflict => {
            AppError::new(StatusCode::CONFLICT, codes::BAD_REQUEST).with_param("idempotency_key")
        }
        store::Error::ParentOwner => not_found(),
        e => AppError::new(StatusCode::CONFLICT, codes::BAD_REQUEST).with_param(e.to_string()),
    }
}
fn upstream(error: &okapi_providers::batch::BatchError) -> AppError {
    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR).with_param(error.code)
}
async fn enabled(state: &AppState) -> bool {
    state
        .setting_cached("image_batches_enabled")
        .await
        .as_ref()
        .as_ref()
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
fn public(row: &Batch) -> Value {
    json!({"id":public_id(row.id),"object":"image.batch","task_name":row.task_name,"model":row.model_name,"provider":row.provider,
        "status":row.state.code(),"item_count":row.item_count,"output_count":row.output_count,"success_count":row.success_count,"fail_count":row.failure_count,
        "hold_amount":Money::from_micros(row.maximum_micro).to_usd_string(),"actual_cost":row.actual_micro.map(|n|Money::from_micros(n).to_usd_string()),
        "currency":"USD","created_at":row.created_at.timestamp(),"settled_at":row.completed_at.map(|t|t.timestamp()),"expires_at":row.expires_at.map(|t|t.timestamp()),
        "downloaded_at":row.downloaded_at.map(|t|t.timestamp()),"cancel_requested":row.cancel_requested,"cleanup_done":row.cleanup_done,
        "parent_batch_id":row.parent_id.map(public_id),"error":row.error_code.as_ref().map(|p|json!({"code":codes::UPSTREAM_ERROR,"param":p})),
        "poll_url":format!("/v1/images/batches/{}",public_id(row.id)),
        "download_url":(row.state.terminal() && row.success_count>0 && !row.delete_requested && !row.cleanup_done && row.expires_at.is_some_and(|t|t>chrono::Utc::now())).then(||format!("/v1/images/batches/{}/download",public_id(row.id)))})
}
fn respond(value: Value, status: StatusCode, id: Uuid) -> Response {
    let mut response = (status, axum::Json(value)).into_response();
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    crate::gateway::error::with_request_id(response, id)
}
pub async fn create(State(state): State<AppState>, req: Request) -> Response {
    let id = Uuid::new_v4();
    match submit(&state, req, id).await {
        Ok(row) => {
            let mut response = respond(public(&row), StatusCode::ACCEPTED, id);
            response
                .headers_mut()
                .insert("retry-after", axum::http::HeaderValue::from_static("3"));
            if let Ok(location) = format!("/v1/images/batches/{}", public_id(row.id)).parse() {
                response.headers_mut().insert("location", location);
            }
            response
        }
        Err(e) => e.into_response_with(Some(id)),
    }
}
// Keep authentication, idempotency, pricing and atomic admission in explicit order.
#[allow(clippy::too_many_lines)]
async fn submit(state: &AppState, req: Request, id: Uuid) -> Result<Batch, AppError> {
    let headers = req.headers().clone();
    let key = crate::gateway::auth::authenticate_data_plane(state, &headers).await?;
    if !enabled(state).await {
        return Err(not_found());
    }
    let _permit = state
        .batch_submit_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("batch_submit_capacity")
        })?;
    let bytes = Bytes::from_request(req, state)
        .await
        .map_err(|e| AppError::new(e.status(), codes::BAD_REQUEST).with_param("body"))?;
    let (input, units, proof) = request::Input::read(&bytes)?;
    let idempotency = headers
        .get("idempotency-key")
        .map(|v| {
            let text = v
                .to_str()
                .map_err(|_| AppError::bad_request().with_param("idempotency_key"))?;
            if text.is_empty() || text.len() > 128 || !text.bytes().all(|v| v.is_ascii_graphic()) {
                return Err(AppError::bad_request().with_param("idempotency_key"));
            }
            Ok(hash(text.as_bytes()))
        })
        .transpose()?;
    if let Some(hash) = &idempotency
        && let Some(old) = store::replay(&state.pg, key.user_id, key.key_id, hash, &proof)
            .await
            .map_err(map_store)?
    {
        return Ok(old);
    }
    crate::gateway::refresh_pricebook_if_newer(state)
        .await
        .map_err(|_| AppError::internal())?;
    let prepared = super::prepare_model(state, &key, &input.model, units).await?;
    crate::gateway::auth::check_member_limit(state, &key).await?;
    let candidates = okapi_store::channels::candidates_for_model(
        &state.pg,
        &prepared.canonical,
        &key.pool_chain(),
        state.master_key.as_deref(),
    )
    .await?;
    let mut candidates = crate::gateway::scheduler::order_candidates(candidates)
        .into_iter()
        .filter(|c| binding::eligible(c, input.provider.as_deref()))
        .collect();
    state
        .retain_margin_ok(&key.group_code, &mut candidates)
        .await;
    let mut available = Vec::new();
    for candidate in candidates {
        if budget_available(state, &candidate).await {
            available.push(candidate);
        }
    }
    let candidates = available;
    let candidate = candidates.first().ok_or_else(|| {
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL)
    })?;
    let payload = input.jsonl(id)?;
    let binding = binding::Binding::capture(state, candidate, &input.metadata, &payload).await?;
    let serialized = binding.encode(state)?;
    let unit = &prepared.unit_quote;
    let ratio = state
        .setting_cached("image_batch_ratio_milli")
        .await
        .as_ref()
        .as_ref()
        .and_then(Value::as_i64)
        .unwrap_or(500);
    if !(0..=10_000).contains(&ratio) {
        return Err(AppError::internal().with_param("image_batch_ratio_milli"));
    }
    let amount = scaled(unit.amount.as_micros(), ratio)?;
    let quote = UnitQuote {
        amount,
        original: unit.original.as_micros(),
        discount: unit
            .original
            .as_micros()
            .checked_sub(amount)
            .ok_or_else(AppError::internal)?,
        list_price: unit.list_price.as_micros(),
        upstream_cost: Some(scaled(
            scaled(unit.list_price.as_micros(), candidate.cost_milli)?,
            500,
        )?),
    };
    let maximum = quote.total(units).map_err(map_store)?.amount;
    let quote = serde_json::to_value(quote).map_err(|_| AppError::internal())?;
    let mut pricing =
        serde_json::to_value(&prepared.quote.snapshot).map_err(|_| AppError::internal())?;
    pricing["batch_ratio_milli"] = json!(ratio);
    let previews: Vec<String> = input
        .items
        .iter()
        .map(|item| {
            item.prompt
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(64)
                .collect()
        })
        .collect();
    let items: Vec<store::NewItem<'_>> = input
        .items
        .iter()
        .zip(&previews)
        .map(|(i, p)| store::NewItem {
            custom_id: &i.custom_id,
            prompt_preview: p,
            outputs: i.output_count,
        })
        .collect();
    let admission = rate::Admission::load(state, &key, &prepared.canonical, units).await;
    let ip = crate::gateway::clients::client_ip(&headers).map(|v| v.to_string());
    let created = store::create_admitted(
        &state.pg,
        store::NewBatch {
            id,
            user_id: key.user_id,
            api_key_id: key.key_id,
            request_hash: &proof,
            idempotency_hash: idempotency.as_deref(),
            task_name: &input.task_name,
            parent_id: input.parent_batch_id.as_deref().map(parse_id).transpose()?,
            model: &prepared.canonical,
            group: &key.group_code,
            provider: &candidate.provider,
            channel_id: candidate.channel_id,
            channel_key_id: candidate.channel_key_id,
            upstream_model: candidate.upstream_model(&prepared.canonical),
            pricing: &pricing,
            unit_quote: &quote,
            maximum: Money::from_micros(maximum),
            input: &payload,
            binding: &serialized,
            items: &items,
            client_ip: ip.as_deref(),
            client_type: crate::gateway::clients::detect_client_type(&headers),
        },
        store::Limits {
            per_key_active: key
                .max_concurrency
                .filter(|n| *n > 0)
                .map_or(8, i64::from)
                .min(8),
            ..store::Limits::default()
        },
        || admission.check(),
    )
    .await
    .map_err(map_store)?;
    match created {
        store::Created::New(row) | store::Created::Existing(row) => Ok(row),
    }
}
fn scaled(amount: i64, ratio: i64) -> Result<i64, AppError> {
    i64::try_from(i128::from(amount) * i128::from(ratio) / 1000).map_err(|_| AppError::internal())
}
async fn budget_available(state: &AppState, candidate: &okapi_store::ChannelCandidate) -> bool {
    match candidate.daily_spend_cap_micro {
        Some(cap) => {
            state
                .sched
                .channel_key_spend_get(candidate.channel_key_id)
                .await
                < cap
        }
        None => true,
    }
}
