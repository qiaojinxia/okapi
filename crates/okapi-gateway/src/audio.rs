//! /v1/audio/*（IMPLEMENTATION §4.4 媒体计费）：
//! - speech：字符独立计价和记录，不进入 Token/TPM；二进制音频回传；
//! - transcriptions：per_call 模式必须（时长无法本地解码）；multipart 解析重组转发，
//!   上游 verbose_json 的 duration 若在则记入快照 media_units（秒，审计用）。

use super::admission::{Origin, Payer};
use super::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{BillingState, TokenUsage};
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Multipart;
use okapi_ledger::SettlementInput;
use okapi_ledger::pg::UsageDimensions;
use okapi_pricing::{Quote, calculate, calculate_characters};
use okapi_providers::registry::MediaSurface;
use okapi_providers::rewrite_model;
use serde::Deserialize;
use serde_json::Value;
use std::time::Instant;
use uuid::Uuid;

#[derive(Deserialize)]
struct SpeechProbe {
    model: String,
    #[serde(default)]
    input: String,
}

/// 候选：注册表里声明了 audio 的渠道（openai / openai_compat / azure）。
async fn first_candidate(
    state: &AppState,
    canonical: &str,
    key: &okapi_store::AuthedKey,
) -> Result<okapi_store::ChannelCandidate, AppError> {
    let candidates = super::candidates::media(state, key, canonical, MediaSurface::Audio).await?;
    // 只发一次（不 failover）：取第一个过得了 key 级 RPM / 当日消费闸的候选
    for cand in candidates {
        if super::key_gate::check(state, &cand).await.is_ok() {
            return Ok(cand);
        }
    }
    Err(AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        codes::NO_AVAILABLE_CHANNEL,
    ))
}

// ---- speech ----

pub async fn speech(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    match handle_speech(&state, &headers, &body, request_id, started).await {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

// 鉴权→字符计费→预扣→转发→结算 线性时序
#[allow(clippy::too_many_lines)]
async fn handle_speech(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: Uuid,
    started: Instant,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    let probe: SpeechProbe = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let meta = super::candidates::resolve_model_cached(state, &probe.model).await?;
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

    // Physical character quantity is confined to quoting; Token counters stay empty.
    let chars = u32::try_from(probe.input.chars().count()).unwrap_or(u32::MAX);
    let usage = TokenUsage::default();
    let book = state.pricebook.load();
    let rules_in = super::rule_inputs::collect(state, &book, key.user_id).await;
    let calc = super::admission::calc_context(&key, &canonical, rules_in, chrono::Utc::now(), None);
    let quote = calculate_characters(&book, &calc, chars)?;
    super::auth::check_member_limit(state, &key).await?;
    super::auth::check_group_rate(state, &key).await?;
    super::admission::check_model_rpm(state, &key, &canonical).await?;

    let super::admission::Reserved {
        pool: reservation_pool,
        source_window,
    } = super::admission::reserve(state, &key, request_id, quote.amount, 0, chrono::Utc::now())
        .await?;

    let origin = Origin::of(headers);
    let mut failure = super::failure::Guard::new(
        state,
        &key,
        request_id,
        &canonical,
        &probe.model,
        "/v1/audio/speech",
        started,
        reservation_pool,
        source_window.as_deref(),
        &origin,
    );
    let cand = match first_candidate(state, &canonical, &key).await {
        Ok(c) => c,
        Err(err) => {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            failure.error(&err);
            return Err(err);
        }
    };
    failure.channel(&cand);
    let upstream_model = cand.upstream_model(&canonical).to_owned();
    let Ok(body_up) = rewrite_model(body, &probe.model, &upstream_model) else {
        let _ = state
            .ledger
            .refund(key.user_id, key.key_id, request_id)
            .await;
        let error = AppError::bad_request();
        failure.error(&error);
        return Err(error);
    };
    match state.openai_speech(&cand, &upstream_model, body_up).await {
        Ok((status, content_type, audio)) => {
            super::key_health::success(state, &cand).await;
            settle(
                state,
                &key,
                Payer::new(
                    state,
                    &key,
                    request_id,
                    &origin,
                    reservation_pool,
                    source_window.as_deref(),
                ),
                &canonical,
                &probe.model,
                "/v1/audio/speech",
                &quote,
                usage,
                started,
                Some(&cand),
                None,
                &mut failure,
            )
            .await
            .inspect_err(|error| failure.error(error))?;
            failure.disarm();
            let mut resp = Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(audio))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            if let Ok(value) = axum::http::HeaderValue::from_str(&request_id.to_string()) {
                resp.headers_mut().insert("x-okapi-request-id", value);
            }
            Ok(resp)
        }
        Err(err) => {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            tracing::warn!(request_id = %request_id, error = %err, "speech 上游失败");
            if err.error_code() != codes::NO_AVAILABLE_CHANNEL {
                super::key_health::failure(
                    state,
                    &cand,
                    err.error_code(),
                    super::upstream_failure::failure_kind_of(&err),
                )
                .await;
            }
            let error = super::account_control::attempt_error(&err);
            failure.error(&error);
            Err(error)
        }
    }
}

// ---- transcriptions ----

pub async fn transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Multipart(multipart): Multipart,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    match handle_transcriptions(
        &state,
        &headers,
        multipart,
        request_id,
        started,
        "/audio/transcriptions",
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// /v1/audio/translations（老 ok-api 面核对补）：与 transcriptions 同构（per_call）。
pub async fn translations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Multipart(multipart): Multipart,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    match handle_transcriptions(
        &state,
        &headers,
        multipart,
        request_id,
        started,
        "/audio/translations",
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

// multipart 解析→per_call 预扣→重组转发→结算 的线性时序
#[allow(clippy::too_many_lines)]
async fn handle_transcriptions(
    state: &AppState,
    headers: &HeaderMap,
    mut multipart: axum::extract::Multipart,
    request_id: Uuid,
    started: Instant,
    path: &str,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;

    // 解析全部 part（file 保留 filename/content-type，重组时 boundary 重生成）
    let mut parts: Vec<(String, Option<String>, Option<String>, Bytes)> = Vec::new();
    let mut model = String::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::bad_request())?
    {
        let name = field.name().unwrap_or_default().to_owned();
        let filename = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(str::to_owned);
        let data = field.bytes().await.map_err(|_| AppError::bad_request())?;
        if name == "model" {
            String::from_utf8_lossy(&data).trim().clone_into(&mut model);
        }
        parts.push((name, filename, content_type, data));
    }
    if model.is_empty() {
        return Err(AppError::bad_request().with_param("model"));
    }

    let meta = super::candidates::resolve_model_cached(state, &model).await?;
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

    // per_call：预扣即终额（时长无法本地解码，§4.4 定案）
    let book = state.pricebook.load();
    let rules_in = super::rule_inputs::collect(state, &book, key.user_id).await;
    let calc = super::admission::calc_context(&key, &canonical, rules_in, chrono::Utc::now(), None);
    let quote = calculate(&book, &calc, TokenUsage::default())?;
    if quote.snapshot.mode != "per_call" {
        return Err(AppError::bad_request().with_param("transcriptions_requires_per_call_model"));
    }
    super::auth::check_member_limit(state, &key).await?;
    super::auth::check_group_rate(state, &key).await?;
    super::admission::check_model_rpm(state, &key, &canonical).await?;

    let super::admission::Reserved {
        pool: reservation_pool,
        source_window,
    } = super::admission::reserve(state, &key, request_id, quote.amount, 0, chrono::Utc::now())
        .await?;

    let endpoint = if path == "/audio/translations" {
        "/v1/audio/translations"
    } else {
        "/v1/audio/transcriptions"
    };
    let origin = Origin::of(headers);
    let mut failure = super::failure::Guard::new(
        state,
        &key,
        request_id,
        &canonical,
        &model,
        endpoint,
        started,
        reservation_pool,
        source_window.as_deref(),
        &origin,
    );
    let cand = match first_candidate(state, &canonical, &key).await {
        Ok(c) => c,
        Err(err) => {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            failure.error(&err);
            return Err(err);
        }
    };
    failure.channel(&cand);
    let upstream_model = cand.upstream_model(&canonical).to_owned();
    // model part 重写为上游名
    for (name, _, _, data) in &mut parts {
        if name == "model" {
            *data = Bytes::from(upstream_model.clone().into_bytes());
        }
    }
    match state
        .openai_audio_multipart(&cand, &upstream_model, path, parts)
        .await
    {
        Ok(resp) => {
            super::key_health::success(state, &cand).await;
            // verbose_json 的 duration（秒）记快照 media_units 供审计
            let duration_secs = serde_json::from_slice::<Value>(&resp.body)
                .ok()
                .and_then(|v| v.get("duration").and_then(Value::as_f64))
                .map(|d| {
                    // 展示/审计用途：向上取整秒（非计费输入）
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let secs = d.ceil() as u32;
                    secs
                });
            settle(
                state,
                &key,
                Payer::new(
                    state,
                    &key,
                    request_id,
                    &origin,
                    reservation_pool,
                    source_window.as_deref(),
                ),
                &canonical,
                &model,
                endpoint,
                &quote,
                TokenUsage::default(),
                started,
                Some(&cand),
                duration_secs,
                &mut failure,
            )
            .await
            .inspect_err(|error| failure.error(error))?;
            failure.disarm();
            let mut out = Response::builder()
                .status(resp.status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(resp.body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            if let Ok(value) = axum::http::HeaderValue::from_str(&request_id.to_string()) {
                out.headers_mut().insert("x-okapi-request-id", value);
            }
            Ok(out)
        }
        Err(err) => {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            tracing::warn!(request_id = %request_id, error = %err, "transcriptions 上游失败");
            if err.error_code() != codes::NO_AVAILABLE_CHANNEL {
                super::key_health::failure(
                    state,
                    &cand,
                    err.error_code(),
                    super::upstream_failure::failure_kind_of(&err),
                )
                .await;
            }
            let error = super::account_control::attempt_error(&err);
            failure.error(&error);
            Err(error)
        }
    }
}

// ---- 结算（speech/transcriptions 共用） ----

#[allow(clippy::too_many_arguments)]
async fn settle(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    payer: Payer<'_>,
    canonical: &str,
    requested_model: &str,
    endpoint: &str,
    quote: &Quote,
    usage: TokenUsage,
    started: Instant,
    cand: Option<&okapi_store::ChannelCandidate>,
    media_units: Option<u32>,
    failure: &mut super::failure::Guard,
) -> Result<(), AppError> {
    let mut snapshot = quote.snapshot.clone();
    if media_units.is_some() {
        snapshot.media_units = media_units;
    }
    let pricing_epoch = snapshot.epoch;
    let mut snapshot = serde_json::to_value(&snapshot).ok();
    if let Some(cand) = cand {
        snapshot = super::upstream_cost::snapshot(
            snapshot,
            cand.channel_id,
            cand.cost_milli,
            quote.list_price,
        );
    }
    let input = SettlementInput {
        log_type: 2,
        channel_id: cand.map(|c| c.channel_id),
        channel_key_id: cand.map(|c| c.channel_key_id),
        state: BillingState::Committed,
        usage,
        amount: quote.amount,
        original: quote.original,
        discount: quote.discount,
        list_price: quote.list_price,
        pricing_epoch: Some(pricing_epoch),
        pricing_snapshot: snapshot,
        latency_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
        upstream_status: Some(200),
        delta_micro: quote.amount.as_micros().saturating_neg(),
        event_type: "commit",
        ..payer.settlement(
            canonical,
            UsageDimensions::new(
                requested_model,
                cand.map_or("", |c| c.upstream_model(canonical)),
                endpoint,
                endpoint,
            ),
        )
    };
    failure.disarm();
    if !state
        .settle_success(input)
        .await
        .inspect_err(|error| failure.settlement_failed(error))?
    {
        return Ok(());
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
