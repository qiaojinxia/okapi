//! /v1/audio/*（IMPLEMENTATION §4.4 媒体计费）：
//! - speech：输入字符数记为 prompt_tokens 走 ratio（或模型配 per_call）；二进制音频回传；
//! - transcriptions：per_call 模式必须（时长无法本地解码）；multipart 解析重组转发，
//!   上游 verbose_json 的 duration 若在则记入快照 media_units（秒，审计用）。

use super::clients::detect_client_type;
use super::error::AppError;
use super::state::AppState;
use axum::body::Body;
use axum::extract::{Multipart, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{BillingState, GroupCode, ModelCode, TokenUsage, UserId};
use okapi_ledger::{CommitOutcome, LimitCaps, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, Quote, RatioFp, calculate};
use okapi_providers::rewrite_model;
use serde::Deserialize;
use serde_json::Value;
use std::time::Instant;
use uuid::Uuid;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";

#[derive(Deserialize)]
struct SpeechProbe {
    model: String,
    #[serde(default)]
    input: String,
}

fn calc_ctx(
    key: &okapi_store::AuthedKey,
    model: &str,
    rules_in: super::rule_inputs::RuleInputs,
) -> CalcContext {
    let now = chrono::Utc::now();
    CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(model),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: rules_in.monthly_tokens,
        local_minute_of_day: u16::try_from((now.timestamp().div_euclid(60)).rem_euclid(1440))
            .unwrap_or(0),
        now_unix: now.timestamp(),
        surge_active: rules_in.surge_active,
        service_tier: None,
    }
}

fn caps_of(key: &okapi_store::AuthedKey) -> LimitCaps {
    let cap = |v: Option<i32>| v.map_or(0, i64::from);
    LimitCaps {
        rpm: cap(key.rpm_limit),
        tpm: cap(key.tpm_limit),
        rpd: cap(key.rpd_limit),
        concurrency: cap(key.max_concurrency),
    }
}

/// 候选（openai 系）：audio 只走 openai/openai_compat 渠道。
async fn first_candidate(
    state: &AppState,
    canonical: &str,
    key: &okapi_store::AuthedKey,
) -> Result<okapi_store::ChannelCandidate, AppError> {
    let rows =
        okapi_store::channels::candidates_for_model(&state.pg, canonical, &key.visibility_groups)
            .await?;
    super::scheduler::order_candidates(rows)
        .into_iter()
        .find(|c| c.provider != "anthropic" && c.provider != "gemini")
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL))
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
    let key = super::auth::authenticate(state, headers).await?;
    let probe: SpeechProbe = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
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

    // 字符数即计费单位（prompt 侧）
    let chars = u32::try_from(probe.input.chars().count()).unwrap_or(u32::MAX);
    let usage = TokenUsage {
        prompt_tokens: chars,
        cached_tokens: 0,
        cache_write_tokens: 0,
        completion_tokens: 0,
        reasoning_tokens: 0,
    };
    let book = state.pricebook.load();
    let rules_in = super::rule_inputs::collect(state, &book, key.user_id).await;
    let calc = calc_ctx(&key, &canonical, rules_in);
    let quote = calculate(&book, &calc, usage)?;
    super::auth::check_member_limit(state, &key).await?;

    match state
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: key.user_id,
                api_key_id: key.key_id,
                request_id,
                est: quote.amount,
                caps: caps_of(&key),
                est_tokens: u64::from(chars),
            },
            chrono::Utc::now(),
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

    let cand = match first_candidate(state, &canonical, &key).await {
        Ok(c) => c,
        Err(err) => {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            return Err(err);
        }
    };
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
        .speech(&base, &cand.credential, body_up)
        .await
    {
        Ok((status, content_type, audio)) => {
            settle(
                state,
                &key,
                &canonical,
                &quote,
                usage,
                request_id,
                started,
                Some(&cand),
                None,
                headers,
            )
            .await;
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
            Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                codes::UPSTREAM_ERROR,
            ))
        }
    }
}

// ---- transcriptions ----

pub async fn transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
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
    multipart: Multipart,
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
    mut multipart: Multipart,
    request_id: Uuid,
    started: Instant,
    path: &str,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate(state, headers).await?;

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

    let meta = super::chat::resolve_model_cached(state, &model).await?;
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
    let calc = calc_ctx(&key, &canonical, rules_in);
    let quote = calculate(&book, &calc, TokenUsage::default())?;
    if quote.snapshot.mode != "per_call" {
        return Err(AppError::bad_request().with_param("transcriptions_requires_per_call_model"));
    }
    super::auth::check_member_limit(state, &key).await?;

    match state
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: key.user_id,
                api_key_id: key.key_id,
                request_id,
                est: quote.amount,
                caps: caps_of(&key),
                est_tokens: 0,
            },
            chrono::Utc::now(),
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

    let cand = match first_candidate(state, &canonical, &key).await {
        Ok(c) => c,
        Err(err) => {
            let _ = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await;
            return Err(err);
        }
    };
    let upstream_model = cand.upstream_model(&canonical).to_owned();
    // model part 重写为上游名
    for (name, _, _, data) in &mut parts {
        if name == "model" {
            *data = Bytes::from(upstream_model.clone().into_bytes());
        }
    }
    let base = cand
        .api_base
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned());

    match state
        .upstream
        .audio_multipart(&base, path, &cand.credential, parts)
        .await
    {
        Ok(resp) => {
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
                &canonical,
                &quote,
                TokenUsage::default(),
                request_id,
                started,
                Some(&cand),
                duration_secs,
                headers,
            )
            .await;
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
            Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                codes::UPSTREAM_ERROR,
            ))
        }
    }
}

// ---- 结算（speech/transcriptions 共用） ----

#[allow(clippy::too_many_arguments)]
async fn settle(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    canonical: &str,
    quote: &Quote,
    usage: TokenUsage,
    request_id: Uuid,
    started: Instant,
    cand: Option<&okapi_store::ChannelCandidate>,
    media_units: Option<u32>,
    headers: &HeaderMap,
) {
    let book = state.pricebook.load();
    match state
        .ledger
        .commit(key.user_id, key.key_id, request_id, quote.amount)
        .await
    {
        Ok(CommitOutcome::Committed { balance_after, .. }) => {
            let mut snapshot = quote.snapshot.clone();
            if media_units.is_some() {
                snapshot.media_units = media_units;
            }
            let input = SettlementInput {
                request_id,
                log_type: 2,
                user_id: key.user_id,
                api_key_id: key.key_id,
                group_code: &key.group_code,
                model_name: canonical,
                channel_id: cand.map(|c| c.channel_id),
                channel_key_id: cand.map(|c| c.channel_key_id),
                state: BillingState::Committed,
                usage,
                amount: quote.amount,
                original: quote.original,
                discount: quote.discount,
                pricing_epoch: Some(book.epoch()),
                pricing_snapshot: serde_json::to_value(&snapshot).ok(),
                latency_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
                ttft_ms: None,
                is_stream: false,
                retry_count: 0,
                failover_count: 0,
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
                usage.total_raw(),
            )
            .await;
        }
        Ok(CommitOutcome::NoReservation) => {
            tracing::warn!(request_id = %request_id, "audio 重复结算竞争，跳过");
        }
        Err(err) => {
            tracing::error!(request_id = %request_id, error = %err, "audio Redis 结算失败（悬置待清理）");
        }
    }
}
