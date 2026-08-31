//! /v1/embeddings：鉴权 → 估价预扣（仅 prompt）→ 候选 failover → 结算。
//! 非流式单跳，复用 chat 同款调度语义（并发槽/状态机/退款兜底），
//! anthropic 渠道无 embeddings 端点，候选层跳过。

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
use okapi_pricing::{CalcContext, RatioFp, calculate};
use okapi_providers::{UpstreamError, rewrite_model};
use okapi_store::channels::KeyFailure;
use serde::Deserialize;
use std::time::Instant;
use uuid::Uuid;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const MAX_ATTEMPTS: usize = 3;

#[derive(Deserialize)]
struct EmbeddingsProbe {
    model: String,
    #[serde(default)]
    input: serde_json::Value,
}

#[derive(Deserialize)]
struct RerankProbe {
    model: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    documents: serde_json::Value,
}

fn input_chars(input: &serde_json::Value) -> usize {
    match input {
        serde_json::Value::String(s) => s.chars().count(),
        serde_json::Value::Array(items) => items.iter().map(input_chars).sum(),
        // token 数组等非文本输入：按元素个数近似
        serde_json::Value::Number(_) => 1,
        _ => 0,
    }
}

pub async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let Ok(probe) = serde_json::from_slice::<EmbeddingsProbe>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    let est = input_chars(&probe.input) / 4 + 3;
    match handle(
        &state,
        &headers,
        &body,
        request_id,
        started,
        &probe.model,
        "/embeddings",
        est,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// /v1/rerank（#1117，Jina/Cohere 兼容形状）：query+documents 文本估算，prompt-only 计费。
pub async fn rerank(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let Ok(probe) = serde_json::from_slice::<RerankProbe>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    let est = (probe.query.chars().count() + input_chars(&probe.documents)) / 4 + 3;
    match handle(
        &state,
        &headers,
        &body,
        request_id,
        started,
        &probe.model,
        "/rerank",
        est,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

// 时序与 chat 主链一致，拆分损害可读性
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn handle(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: Uuid,
    started: Instant,
    requested_model: &str,
    upstream_path: &str,
    est_chars_based: usize,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate(state, headers).await?;

    let meta = super::chat::resolve_model_cached(state, requested_model).await?;
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
        local_minute_of_day: minute_of_day,
        now_unix: now.timestamp(),
        surge_active: rules_in.surge_active,
        service_tier: None,
    };

    // 估算：仅 prompt 侧（chars/4 启发）
    let est_prompt = u32::try_from(est_chars_based).unwrap_or(u32::MAX);
    let est_usage = TokenUsage {
        prompt_tokens: est_prompt,
        cached_tokens: 0,
        cache_write_tokens: 0,
        audio_prompt_tokens: 0,
        image_prompt_tokens: 0,
        completion_tokens: 0,
        audio_completion_tokens: 0,
        reasoning_tokens: 0,
    };
    let est_quote = calculate(&book, &calc, est_usage)?;
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
                est: est_quote.amount,
                caps,
                est_tokens: u64::from(est_prompt),
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

    // —— 预扣已建立：一切失败路径必须退款 ——
    match forward(
        state,
        &key,
        &canonical,
        requested_model,
        upstream_path,
        body,
        request_id,
    )
    .await
    {
        Ok((resp_body, status, usage, channel, upstream_request_id, failover)) => {
            let usage = usage.unwrap_or(TokenUsage {
                prompt_tokens: est_prompt,
                cached_tokens: 0,
                cache_write_tokens: 0,
                audio_prompt_tokens: 0,
                image_prompt_tokens: 0,
                completion_tokens: 0,
                audio_completion_tokens: 0,
                reasoning_tokens: 0,
            });
            let quote = calculate(&book, &calc, usage).map_err(AppError::from);
            match quote {
                Ok(quote) => {
                    match state
                        .ledger
                        .commit(key.user_id, key.key_id, request_id, quote.amount)
                        .await
                    {
                        Ok(CommitOutcome::Committed { balance_after, .. }) => {
                            let snapshot = serde_json::to_value(&quote.snapshot).ok();
                            let input = SettlementInput {
                                request_id,
                                log_type: 2,
                                user_id: key.user_id,
                                api_key_id: key.key_id,
                                group_code: &key.group_code,
                                model_name: &canonical,
                                channel_id: Some(channel.0),
                                channel_key_id: Some(channel.1),
                                state: BillingState::Committed,
                                usage,
                                amount: quote.amount,
                                original: quote.original,
                                discount: quote.discount,
                                pricing_epoch: Some(book.epoch()),
                                pricing_snapshot: snapshot,
                                latency_ms: elapsed_ms(started),
                                ttft_ms: None,
                                is_stream: false,
                                retry_count: 0,
                                failover_count: failover,
                                upstream_status: Some(200),
                                error_code: None,
                                upstream_request_id: upstream_request_id.as_deref(),
                                node: state.node.as_ref(),
                                sticky_layer: 3,
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
                            tracing::warn!(request_id = %request_id, "embeddings 重复结算竞争，跳过");
                        }
                        Err(err) => {
                            tracing::error!(request_id = %request_id, error = %err, "embeddings Redis 结算失败（悬置待清理）");
                        }
                    }
                }
                Err(err) => {
                    let _ = state
                        .ledger
                        .refund(key.user_id, key.key_id, request_id)
                        .await;
                    return Err(err);
                }
            }
            let resp = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(resp_body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            Ok(with_request_id(resp, request_id))
        }
        Err((err, channel, failover)) => {
            if let Err(rerr) = state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await
            {
                tracing::error!(request_id = %request_id, error = %rerr, "embeddings 退款失败（悬置待清理）");
            }
            let input = SettlementInput {
                request_id,
                log_type: 5,
                user_id: key.user_id,
                api_key_id: key.key_id,
                group_code: &key.group_code,
                model_name: &canonical,
                channel_id: channel.map(|c| c.0),
                channel_key_id: channel.map(|c| c.1),
                state: BillingState::Failed,
                usage: TokenUsage::default(),
                amount: Money::ZERO,
                original: Money::ZERO,
                discount: Money::ZERO,
                pricing_epoch: Some(book.epoch()),
                pricing_snapshot: None,
                latency_ms: elapsed_ms(started),
                ttft_ms: None,
                is_stream: false,
                retry_count: 0,
                failover_count: failover,
                upstream_status: None,
                error_code: Some(err.code.as_str()),
                upstream_request_id: None,
                node: state.node.as_ref(),
                sticky_layer: 0,
                client_type: detect_client_type(headers),
                client_ip: None,
                delta_micro: 0,
                balance_after: None,
                event_type: "refund",
            };
            state.settle_write(input).await;
            Err(err)
        }
    }
}

type ForwardOk = (
    Bytes,
    u16,
    Option<TokenUsage>,
    (i64, i64),
    Option<String>,
    i16,
);

/// 候选循环：anthropic 渠道跳过（无 embeddings 端点）；瞬态失败 failover。
#[allow(clippy::too_many_arguments)]
async fn forward(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    canonical: &str,
    requested_model: &str,
    upstream_path: &str,
    body: &Bytes,
    request_id: Uuid,
) -> Result<ForwardOk, (AppError, Option<(i64, i64)>, i16)> {
    let rows =
        okapi_store::channels::candidates_for_model(&state.pg, canonical, key.pool_code.as_deref())
            .await
            .map_err(|e| (AppError::from(e), None, 0))?;
    let candidates: Vec<_> = super::scheduler::order_candidates(rows)
        .into_iter()
        .filter(|c| c.provider != "anthropic")
        .collect();
    if candidates.is_empty() {
        return Err((
            AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL),
            None,
            0,
        ));
    }

    let mut failover: i16 = 0;
    let mut last: Option<(i64, i64)> = None;
    for cand in candidates.into_iter().take(MAX_ATTEMPTS) {
        let upstream_model = cand.upstream_model(canonical).to_owned();
        let Ok(body_up) = rewrite_model(body, requested_model, &upstream_model) else {
            return Err((
                AppError::bad_request(),
                Some((cand.channel_id, cand.channel_key_id)),
                failover,
            ));
        };
        let base = cand
            .api_base
            .clone()
            .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned());
        last = Some((cand.channel_id, cand.channel_key_id));

        match state
            .upstream
            .json_relay(&base, upstream_path, &cand.credential, body_up)
            .await
        {
            Ok(resp) => {
                return Ok((
                    resp.body,
                    resp.status,
                    resp.usage.map(okapi_api::UsageProbe::to_token_usage),
                    (cand.channel_id, cand.channel_key_id),
                    resp.upstream_request_id,
                    failover,
                ));
            }
            Err(err) if err.retriable_before_first_token() => {
                let kind = match &err {
                    UpstreamError::Status { status: 429, .. } => KeyFailure::RateLimited {
                        retry_after_secs: err.retry_after_secs(),
                    },
                    UpstreamError::Status {
                        status: 401 | 403, ..
                    } => KeyFailure::Invalid,
                    _ => KeyFailure::Transient,
                };
                let _ = okapi_store::channels::mark_key_failure(
                    &state.pg,
                    cand.channel_key_id,
                    err.error_code(),
                    kind,
                )
                .await;
                tracing::warn!(request_id = %request_id, channel_key = cand.channel_key_id,
                    code = err.error_code(), "embeddings 失败，failover 下一候选");
                failover = failover.saturating_add(1);
            }
            Err(UpstreamError::Status { status, .. }) => {
                return Err((
                    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                        .with_param(format!("upstream_status_{status}")),
                    last,
                    failover,
                ));
            }
            Err(_) => {
                return Err((
                    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
                    last,
                    failover,
                ));
            }
        }
    }
    Err((
        AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
        last,
        failover,
    ))
}

fn with_request_id(mut resp: Response, request_id: Uuid) -> Response {
    if let Ok(value) = axum::http::HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut().insert("x-okapi-request-id", value);
    }
    resp
}

fn elapsed_ms(started: Instant) -> i32 {
    i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}
