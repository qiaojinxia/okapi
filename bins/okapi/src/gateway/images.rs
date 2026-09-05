//! /v1/images/generations（IMPLEMENTATION §4.4 媒体计费）：
//! per_call × n 张（n clamp 1..10），乘数落 pricing_snapshot.media_units
//! 保账单可解释；仅路由 openai 系渠道。

use super::clients::detect_client_type;
use super::error::AppError;
use super::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{BillingState, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_ledger::{CommitOutcome, LimitCaps, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, Quote, RatioFp, calculate};
use okapi_providers::rewrite_model;
use serde::Deserialize;
use std::time::Instant;
use uuid::Uuid;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const MAX_ATTEMPTS: usize = 3;
const MAX_IMAGES: u32 = 10;

#[derive(Deserialize)]
struct ImagesProbe {
    model: String,
    #[serde(default)]
    n: Option<u32>,
}

pub async fn images(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    match handle(&state, &headers, &body, request_id, started).await {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// per_call 报价 × 张数（整数饱和乘，乘数记入快照）。
fn scale_quote(quote: &Quote, units: u32) -> Quote {
    let n = i64::from(units);
    let mut snapshot = quote.snapshot.clone();
    snapshot.media_units = Some(units);
    Quote {
        amount: Money::from_micros(quote.amount.as_micros().saturating_mul(n)),
        original: Money::from_micros(quote.original.as_micros().saturating_mul(n)),
        discount: Money::from_micros(quote.discount.as_micros().saturating_mul(n)),
        list_price: Money::from_micros(quote.list_price.as_micros().saturating_mul(n)),
        snapshot,
    }
}

// 时序与 chat 主链一致
#[allow(clippy::too_many_lines)]
async fn handle(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: Uuid,
    started: Instant,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    let probe: ImagesProbe = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let units = probe.n.unwrap_or(1).clamp(1, MAX_IMAGES);

    let meta = super::chat::resolve_model_cached(state, &probe.model).await?;
    let Some(meta) = meta.as_ref() else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND));
    };
    let canonical = meta.canonical.clone();
    if !key.allows_model(&canonical) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::MODEL_NOT_ALLOWED,
        ));
    }

    let book = state.pricebook.load();
    let rules_in = super::rule_inputs::collect(state, &book, key.user_id).await;
    let now = chrono::Utc::now();
    let minute_of_day =
        u16::try_from((now.timestamp().div_euclid(60)).rem_euclid(1440)).unwrap_or(0);
    let calc = CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(canonical.as_str()),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: rules_in.monthly_tokens,
        monthly_spend_micro: rules_in.monthly_spend_micro,
        local_minute_of_day: minute_of_day,
        now_unix: now.timestamp(),
        surge_active: rules_in.surge_active,
        service_tier: None,
    };
    let quote = scale_quote(&calculate(&book, &calc, TokenUsage::default())?, units);
    super::auth::check_member_limit(state, &key).await?;

    let cap = |v: Option<i32>| v.map_or(0, i64::from);
    let caps = LimitCaps {
        rpm: cap(key.rpm_limit),
        tpm: cap(key.tpm_limit),
        rpd: cap(key.rpd_limit),
        concurrency: cap(key.max_concurrency),
    };
    match state
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: key.user_id,
                api_key_id: key.key_id,
                request_id,
                est: quote.amount,
                caps,
                est_tokens: 0,
            },
            now,
        )
        .await?
    {
        ReserveOutcome::Reserved { .. } => {}
        ReserveOutcome::Insufficient { .. } => {
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                codes::INSUFFICIENT_QUOTA,
            ));
        }
        ReserveOutcome::RateLimited { which } => {
            return Err(
                AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(which),
            );
        }
    }

    // —— 预扣已建立 ——
    let rows = okapi_store::channels::candidates_for_model(
        &state.pg,
        &canonical,
        &key.pool_chain(),
        state.master_key.as_deref(),
    )
    .await
    .map_err(AppError::from);
    let candidates: Vec<_> = match rows {
        Ok(rows) => super::scheduler::order_candidates(rows)
            .into_iter()
            .filter(|c| c.provider != "anthropic" && c.provider != "gemini")
            .collect(),
        Err(err) => {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            return Err(err);
        }
    };
    if candidates.is_empty() {
        let _ = state
            .ledger
            .refund(key.user_id, key.key_id, request_id)
            .await;
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::NO_AVAILABLE_CHANNEL,
        ));
    }

    let mut failover: i16 = 0;
    let mut last_err: Option<AppError> = None;
    for cand in candidates.into_iter().take(MAX_ATTEMPTS) {
        let upstream_model = cand.upstream_model(&canonical).to_owned();
        let Ok(body_up) = rewrite_model(body, &probe.model, &upstream_model) else {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            return Err(AppError::bad_request());
        };
        let base = cand
            .api_base
            .clone()
            .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned());
        match state
            .upstream
            .images(&base, &cand.credential, body_up)
            .await
        {
            Ok(resp) => {
                commit_and_record(
                    state,
                    &key,
                    &canonical,
                    &probe.model,
                    &quote,
                    units,
                    request_id,
                    started,
                    &cand,
                    failover,
                    headers,
                )
                .await;
                let out = Response::builder()
                    .status(resp.status)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(resp.body))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                return Ok(out);
            }
            Err(err) if err.retriable_before_first_token() => {
                let _ = okapi_store::channels::mark_key_failure(
                    &state.pg,
                    cand.channel_key_id,
                    err.error_code(),
                    okapi_store::channels::KeyFailure::Transient,
                )
                .await;
                failover = failover.saturating_add(1);
                last_err = Some(AppError::new(
                    StatusCode::BAD_GATEWAY,
                    codes::UPSTREAM_ERROR,
                ));
            }
            Err(_) => {
                last_err = Some(AppError::new(
                    StatusCode::BAD_GATEWAY,
                    codes::UPSTREAM_ERROR,
                ));
                break;
            }
        }
    }

    if let Err(err) = state
        .ledger
        .refund(key.user_id, key.key_id, request_id)
        .await
    {
        tracing::error!(request_id = %request_id, error = %err, "images 退款失败（悬置待清理）");
    }
    Err(last_err.unwrap_or_else(|| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)))
}

#[allow(clippy::too_many_arguments)]
async fn commit_and_record(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    canonical: &str,
    requested_model: &str,
    quote: &Quote,
    _units: u32,
    request_id: Uuid,
    started: Instant,
    cand: &okapi_store::ChannelCandidate,
    failover: i16,
    headers: &HeaderMap,
) {
    let book = state.pricebook.load();
    match state
        .ledger
        .commit(key.user_id, key.key_id, request_id, quote.amount)
        .await
    {
        Ok(CommitOutcome::Committed { balance_after, .. }) => {
            let input = SettlementInput {
                dimensions: okapi_ledger::pg::UsageDimensions::new(
                    requested_model,
                    cand.upstream_model(canonical),
                    "/v1/images/generations",
                    "/v1/images/generations",
                ),
                request_id,
                log_type: 2,
                user_id: key.user_id,
                api_key_id: key.key_id,
                group_code: &key.group_code,
                model_name: canonical,
                channel_id: Some(cand.channel_id),
                channel_key_id: Some(cand.channel_key_id),
                state: BillingState::Committed,
                usage: TokenUsage::default(),
                amount: quote.amount,
                original: quote.original,
                discount: quote.discount,
                list_price: quote.list_price,
                upstream_cost: None,
                pricing_epoch: Some(book.epoch()),
                pricing_snapshot: serde_json::to_value(&quote.snapshot).ok(),
                latency_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
                ttft_ms: None,
                is_stream: false,
                retry_count: 0,
                failover_count: failover,
                upstream_status: Some(200),
                error_code: None,
                upstream_request_id: None,
                node: state.node.as_ref(),
                sticky_layer: 0,
                client_type: detect_client_type(headers),
                client_ip: None,
                delta_micro: quote.amount.as_micros().saturating_neg(),
                balance_after: Some(balance_after),
                event_type: "commit",
            };
            state.settle_write(input).await;
            super::auth::record_settlement_counters(
                state,
                key.user_id,
                key.member_user_id,
                quote.amount.as_micros(),
                0,
            )
            .await;
        }
        Ok(CommitOutcome::NoReservation) => {
            tracing::warn!(request_id = %request_id, "images 重复结算竞争，跳过");
        }
        Err(err) => {
            tracing::error!(request_id = %request_id, error = %err, "images Redis 结算失败（悬置待清理）");
        }
    }
}
