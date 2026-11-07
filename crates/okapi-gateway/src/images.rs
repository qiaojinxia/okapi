//! /v1/images/generations 与 /v1/images/edits（IMPLEMENTATION §4.4 媒体计费）：
//! per_call 按成功张数，ratio/tiered 按响应总用量；价簿固定于准入时。
//! 仅路由 openai 系渠道。edits 支持 JSON 与 multipart，共用验证、路由和结算。

use super::admission::{Origin, Payer};
use super::state::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okapi_api::codes;
use okapi_domain::{BillingState, Money, TokenUsage};
use okapi_kernel::error::AppError;
use okapi_kernel::error::with_request_id;
use okapi_ledger::SettlementInput;
use okapi_pricing::{CalcContext, PriceBook, Quote, calculate};
use okapi_providers::UpstreamError;
use okapi_providers::registry::MediaSurface;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

const MAX_ATTEMPTS: usize = 3;
pub mod batches;
mod request;
mod stream;
pub mod tasks;
mod usage;

pub async fn edits(State(state): State<AppState>, req: Request) -> Response {
    Box::pin(receive(state, req, "/v1/images/edits")).await
}

pub async fn images(State(state): State<AppState>, req: Request) -> Response {
    Box::pin(receive(state, req, "/v1/images/generations")).await
}

async fn receive(state: AppState, req: Request, endpoint: &str) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let headers = req.headers().clone();
    let result = async {
        let key = super::auth::authenticate_data_plane(&state, &headers).await?;
        let input = request::read(req, &state, endpoint == "/v1/images/edits").await?;
        handle(
            &state, &key, &headers, input, request_id, started, endpoint, None,
        )
        .await
    }
    .await;
    match result {
        Ok(resp) => with_request_id(resp, request_id),
        Err(error) => error.into_response_with(Some(request_id)),
    }
}

/// per_call 报价 × 张数（整数检查乘，乘数记入快照）。
fn scale_quote(quote: &Quote, units: u32) -> Result<Quote, AppError> {
    let n = i64::from(units);
    let mut snapshot = quote.snapshot.clone();
    snapshot.media_units = Some(units);
    let scale = |value: Money| {
        value
            .as_micros()
            .checked_mul(n)
            .map(Money::from_micros)
            .ok_or_else(|| AppError::from(okapi_pricing::PricingError::Overflow))
    };
    Ok(Quote {
        amount: scale(quote.amount)?,
        original: scale(quote.original)?,
        discount: scale(quote.discount)?,
        list_price: scale(quote.list_price)?,
        snapshot,
    })
}

struct Prepared {
    canonical: String,
    unit_quote: Quote,
    quote: Quote,
    pricing_epoch: i64,
    book: Arc<PriceBook>,
    calc: CalcContext,
    estimated_tokens: u64,
}

async fn prepare(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    input: &request::Input,
) -> Result<Prepared, AppError> {
    prepare_pricing(state, key, &input.model, input.units, Some(input)).await
}

async fn prepare_model(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    model: &str,
    units: u32,
) -> Result<Prepared, AppError> {
    // Native batch receipts still represent per-image prices, not token prices.
    prepare_pricing(state, key, model, units, None).await
}

async fn prepare_pricing(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    model: &str,
    units: u32,
    input: Option<&request::Input>,
) -> Result<Prepared, AppError> {
    let meta = super::candidates::resolve_model_cached(state, model).await?;
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
    let calc = super::admission::calc_context(key, canonical.as_str(), rules_in, now, None);
    let unit_quote = calculate(&book, &calc, TokenUsage::default())?;
    let estimated = if let Some(input) = input {
        let output = meta
            .max_output
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0);
        input.estimate(output)?
    } else {
        TokenUsage::default()
    };
    let quote = if unit_quote.snapshot.mode == "per_call" {
        scale_quote(&unit_quote, units)?
    } else {
        if input.is_none() {
            return Err(AppError::bad_request().with_param("images_requires_per_call_model"));
        }
        calculate(&book, &calc, estimated)?
    };
    let pricing_epoch = book.epoch();
    Ok(Prepared {
        canonical,
        unit_quote,
        quote,
        pricing_epoch,
        book,
        calc,
        estimated_tokens: estimated.total_raw(),
    })
}

async fn refund(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    request_id: Uuid,
    task: Option<tasks::Lease>,
) -> Result<(), AppError> {
    if task.is_none() {
        state
            .ledger
            .refund(key.user_id, key.key_id, request_id)
            .await?;
    }
    Ok(())
}

// 时序与 chat 主链一致
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn handle(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    headers: &HeaderMap,
    input: request::Input,
    request_id: Uuid,
    started: Instant,
    endpoint: &str,
    task: Option<tasks::Lease>,
) -> Result<Response, AppError> {
    let model = &input.model;
    let units = input.units;
    let Prepared {
        canonical,
        unit_quote,
        quote,
        pricing_epoch,
        book,
        calc,
        estimated_tokens,
    } = prepare(state, key, &input).await?;
    let now = chrono::Utc::now();
    super::auth::check_member_limit(state, key).await?;
    super::auth::check_group_rate(state, key).await?;
    super::admission::check_model_rpm(state, key, &canonical).await?;

    let super::admission::Reserved {
        pool: reserved_pool,
        source_window,
    } = super::admission::reserve(state, key, request_id, quote.amount, estimated_tokens, now)
        .await?;

    let origin = Origin::of(headers);
    let mut failure = super::failure::Guard::new(
        state,
        key,
        request_id,
        &canonical,
        model,
        endpoint,
        started,
        reserved_pool,
        source_window.as_deref(),
        &origin,
    );
    // —— 预扣已建立 ——
    let candidates =
        match super::candidates::media(state, key, &canonical, MediaSurface::Images).await {
            Ok(candidates) => candidates,
            Err(error) => {
                let _ = refund(state, key, request_id, task).await;
                failure.error(&error);
                return Err(error);
            }
        };

    if input.stream {
        // The tracked stream pump owns terminal cleanup from this point.
        failure.disarm();
        return stream::start(
            stream::Context {
                state: state.clone(),
                key: key.clone(),
                origin,
                input,
                request_id,
                started,
                endpoint: endpoint.to_owned(),
                reserved_pool,
                source_window,
                prepared: Prepared {
                    canonical,
                    unit_quote,
                    quote,
                    pricing_epoch,
                    book,
                    calc,
                    estimated_tokens,
                },
            },
            candidates,
        )
        .await;
    }

    let mut failover: i16 = 0;
    let mut last_err: Option<AppError> = None;
    let mut attempted = 0;
    for cand in candidates {
        if attempted >= MAX_ATTEMPTS {
            break;
        }
        // key 级 RPM / 当日消费闸挡下的候选不算一次尝试（与 chat 同一套闸）
        if super::key_gate::check(state, &cand).await.is_err() {
            continue;
        }
        attempted += 1;
        failure.channel(&cand);
        let upstream_model = cand.upstream_model(&canonical).to_owned();
        if let Some(task) = task
            && !okapi_store::image_tasks::dispatch(
                &state.pg,
                task.id,
                task.token,
                cand.channel_id,
                cand.channel_key_id,
            )
            .await
            .map_err(AppError::from)
            .inspect_err(|error| failure.error(error))?
        {
            let error = AppError::new(StatusCode::CONFLICT, codes::BAD_REQUEST)
                .with_param("image_task_not_dispatchable");
            failure.error(&error);
            return Err(error);
        }
        match input.forward(state, &cand, &upstream_model, endpoint).await {
            Ok(resp) => {
                let actual = match request::returned_images(&resp.body, units) {
                    Ok(actual) => {
                        super::key_health::success(state, &cand).await;
                        actual
                    }
                    Err(error) => {
                        let _ = refund(state, key, request_id, task).await;
                        failure.error(&error);
                        return Err(error);
                    }
                };
                let actual_billing = (|| {
                    let per_image = unit_quote.snapshot.mode == "per_call";
                    let usage = usage::parse(&resp.body, !per_image)?;
                    let mut quote = if per_image {
                        scale_quote(&unit_quote, actual)?
                    } else {
                        calculate(&book, &calc, usage.ok_or_else(usage::invalid)?)?
                    };
                    quote.snapshot.media_units = Some(actual);
                    Ok::<_, AppError>((quote, usage))
                })();
                let (actual_quote, actual_usage) = match actual_billing {
                    Ok(billing) => billing,
                    Err(error) => {
                        let _ = refund(state, key, request_id, task).await;
                        failure.error(&error);
                        return Err(error);
                    }
                };
                commit_and_record(
                    state,
                    key,
                    &canonical,
                    model,
                    &actual_quote,
                    actual_usage,
                    pricing_epoch,
                    started,
                    &cand,
                    failover,
                    endpoint,
                    &resp,
                    task,
                    Payer::new(
                        state,
                        key,
                        request_id,
                        &origin,
                        reserved_pool,
                        source_window.as_deref(),
                    ),
                    None,
                    Some(&mut failure),
                )
                .await
                .inspect_err(|error| failure.error(error))?;
                failure.disarm();
                let out = Response::builder()
                    .status(resp.status)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(resp.body))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                return Ok(out);
            }
            Err(err) if err.error_code() == codes::NO_AVAILABLE_CHANNEL => {
                // Admission denied before send; safe to try the next channel.
                failover = failover.saturating_add(1);
                last_err = Some(super::account_control::attempt_error(&err));
            }
            Err(err)
                if matches!(
                    err,
                    UpstreamError::Status {
                        status: 401 | 402 | 403 | 429,
                        ..
                    }
                ) =>
            {
                super::key_health::failure(
                    state,
                    &cand,
                    err.error_code(),
                    super::upstream_failure::failure_kind_of(&err),
                )
                .await;
                failover = failover.saturating_add(1);
                last_err = Some(super::account_control::attempt_error(&err));
            }
            Err(err) => {
                // 5xx / 超时 / 连接失败不换渠道（图片请求非幂等，重发可能让上游重复出图），
                // 但 key 健康照常登记：连续失败后冷却，后面的请求才会去别的渠道
                if err.retriable_before_first_token() {
                    super::key_health::failure(
                        state,
                        &cand,
                        err.error_code(),
                        super::upstream_failure::failure_kind_of(&err),
                    )
                    .await;
                }
                last_err = Some(super::account_control::attempt_error(&err));
                break;
            }
        }
    }

    if let Err(err) = refund(state, key, request_id, task).await {
        tracing::error!(request_id = %request_id, error = ?err, "images 退款失败（悬置待清理）");
    }
    let error = last_err.unwrap_or_else(|| {
        if attempted == 0 {
            AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL)
        } else {
            AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
        }
    });
    failure.error(&error);
    Err(error)
}

#[allow(clippy::too_many_arguments)]
async fn commit_and_record(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    canonical: &str,
    requested_model: &str,
    quote: &Quote,
    usage: Option<TokenUsage>,
    pricing_epoch: i64,
    started: Instant,
    cand: &okapi_store::ChannelCandidate,
    failover: i16,
    endpoint: &str,
    upstream: &okapi_providers::openai::EmbeddingsResponse,
    task: Option<tasks::Lease>,
    payer: Payer<'_>,
    stream: Option<stream::Receipt>,
    failure: Option<&mut super::failure::Guard>,
) -> Result<(), AppError> {
    let ingress = if task.is_some() {
        format!("{endpoint}/async")
    } else {
        endpoint.into()
    };
    let mut snapshot = serde_json::to_value(&quote.snapshot).map_err(|_| AppError::internal())?;
    super::upstream_cost::pin(
        &mut snapshot,
        cand.channel_id,
        cand.cost_milli,
        quote.list_price,
    );
    usage::annotate(&mut snapshot, usage);
    if let Some(stream) = &stream {
        snapshot["image_stream_usage"] = serde_json::json!(stream.usage_mode);
        snapshot["image_stream_usage_complete"] = serde_json::json!(stream.usage_complete);
        snapshot["image_stream_incomplete"] = serde_json::json!(stream.incomplete);
    }
    let usage = usage.unwrap_or_default();
    let mut input = SettlementInput {
        log_type: 2,
        channel_id: Some(cand.channel_id),
        channel_key_id: Some(cand.channel_key_id),
        state: BillingState::Committed,
        usage,
        amount: quote.amount,
        original: quote.original,
        discount: quote.discount,
        list_price: quote.list_price,
        pricing_epoch: Some(pricing_epoch),
        pricing_snapshot: Some(snapshot),
        latency_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
        ttft_ms: stream.as_ref().and_then(|stream| stream.ttft_ms),
        is_stream: stream.is_some(),
        failover_count: failover,
        upstream_status: i16::try_from(upstream.status).ok(),
        upstream_request_id: upstream.upstream_request_id.as_deref(),
        delta_micro: quote.amount.as_micros().saturating_neg(),
        event_type: "commit",
        ..payer.settlement(
            canonical,
            okapi_ledger::pg::UsageDimensions::new(
                requested_model,
                cand.upstream_model(canonical),
                &ingress,
                endpoint,
            ),
        )
    };
    if let Some(task) = task {
        state.prepare_settlement(&mut input).await;
        tasks::complete(state, task, &upstream.body, input).await?;
        state
            .sched
            .kpi_record(usage.total_raw(), quote.amount.as_micros(), false)
            .await;
    } else {
        let mut failure = failure;
        if let Some(failure) = failure.as_deref_mut() {
            failure.disarm();
        }
        let settled = state.settle_success(input).await.inspect_err(|error| {
            if let Some(failure) = failure {
                failure.settlement_failed(error);
            }
        })?;
        if !settled {
            return Ok(());
        }
    }
    super::auth::record_settlement_counters(
        state,
        key.user_id,
        key.member_user_id,
        quote.amount.as_micros(),
        usage.total_raw(),
    )
    .await;
    Ok(())
}
