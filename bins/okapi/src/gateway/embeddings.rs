//! /v1/embeddings：鉴权 → 估价预扣（仅 prompt）→ 候选 failover → 结算。
//! 非流式单跳，复用 chat 同款调度语义（并发槽/状态机/退款兜底），
//! anthropic 渠道无 embeddings 端点，候选层跳过。

use super::clients::detect_client_type;
use super::error::AppError;
use super::error::with_request_id;
use super::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{BillingState, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_ledger::{LimitCaps, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, RatioFp, calculate};
use okapi_providers::{UpstreamError, rewrite_model};
use serde::Deserialize;
use std::time::Instant;
use uuid::Uuid;

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

/// 估算 embeddings/rerank 输入的 prompt tokens（chat 链路同款 tiktoken 计数）。
///
/// 取代 chars/4 启发：那个式子对中文能差三到四倍（405 汉字 ≈ 108 tokens），预扣
/// 失准，且上游不返 usage 时它就是计费口径、直接少收。分词器按模型名选
/// （`estimate::encoding_for`，OpenAI 系 embedding 模型走 cl100k）。
/// 数字元素是预分词的 token id 序列，个数即约 token 数；其余非文本不计。
fn estimate_input_tokens(model: &str, inputs: &[&serde_json::Value]) -> u32 {
    fn collect<'a>(input: &'a serde_json::Value, texts: &mut Vec<&'a str>, raw_tokens: &mut usize) {
        match input {
            serde_json::Value::String(s) => texts.push(s.as_str()),
            serde_json::Value::Number(_) => *raw_tokens += 1,
            serde_json::Value::Array(items) => {
                for item in items {
                    collect(item, texts, raw_tokens);
                }
            }
            _ => {}
        }
    }
    let mut texts: Vec<&str> = Vec::new();
    let mut raw_tokens = 0usize;
    for input in inputs {
        collect(input, &mut texts, &mut raw_tokens);
    }
    // 无 chat 协议结构：message_count=0，只保留请求级常量（与旧式 +3 对齐）
    super::estimate::estimate_prompt_tokens(model, &texts, 0)
        .saturating_add(u32::try_from(raw_tokens).unwrap_or(u32::MAX))
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
    let est = estimate_input_tokens(&probe.model, &[&probe.input]);
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
    let query = serde_json::Value::String(probe.query);
    let est = estimate_input_tokens(&probe.model, &[&query, &probe.documents]);
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
    est_prompt: u32,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;

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
    let minute_of_day = u16::try_from(
        (now.timestamp()
            .saturating_add(i64::from(
                now.with_timezone(&chrono::Local).offset().local_minus_utc(),
            ))
            .div_euclid(60))
        .rem_euclid(1440),
    )
    .unwrap_or(0);
    let calc = CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(canonical.as_str()),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: rules_in.monthly_tokens,
        monthly_spend_micro: rules_in.monthly_spend_micro,
        local_minute_of_day: minute_of_day,
        now_unix: now.timestamp(),
        utc_offset_seconds: now.with_timezone(&chrono::Local).offset().local_minus_utc(),
        surge_active: rules_in.surge_active,
        service_tier: None,
    };

    // 估算：仅 prompt 侧（tiktoken，chat 链路同款）
    let est_usage = TokenUsage {
        prompt_tokens: est_prompt,
        cached_tokens: 0,
        cache_read_reported: false,
        cache_write_reported: false,
        cache_write_tokens: 0,
        audio_prompt_tokens: 0,
        image_prompt_tokens: 0,
        completion_tokens: 0,
        audio_completion_tokens: 0,
        reasoning_tokens: 0,
        ..TokenUsage::default()
    };
    let est_quote = calculate(&book, &calc, est_usage)?;
    super::auth::check_member_limit(state, &key).await?;
    super::auth::check_group_rate(state, &key).await?;

    let cap = |v: Option<i32>| v.map_or(0, i64::from);
    let caps = LimitCaps {
        rpm: cap(key.rpm_limit),
        tpm: cap(key.tpm_limit),
        rpd: cap(key.rpd_limit),
        concurrency: cap(key.max_concurrency),
    };
    let (reservation_pool, source_window) = match state
        .reserve_for_key(
            key.quota_limited,
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
        ReserveOutcome::Reserved {
            pool,
            source_window,
            ..
        } => (pool, source_window),
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
    };

    let mut failure = super::failure::Guard::new(
        state,
        &key,
        request_id,
        &canonical,
        requested_model,
        &format!("/v1{upstream_path}"),
        started,
        reservation_pool,
        source_window.as_deref(),
    );
    // —— 预扣已建立：一切失败路径必须退款与落失败账单 ——
    match forward(
        state,
        &key,
        &canonical,
        requested_model,
        upstream_path,
        body,
        request_id,
        est_prompt,
    )
    .await
    {
        Ok((resp_body, status, usage, channel, upstream_request_id, failover, upstream_model)) => {
            failure.upstream(&upstream_model, (channel.0, channel.1));
            let usage = usage.unwrap_or(TokenUsage {
                upstream_usage: Some(okapi_domain::UpstreamTokenCounts::default()),
                prompt_tokens: est_prompt,
                cached_tokens: 0,
                cache_read_reported: false,
                cache_write_reported: false,
                cache_write_tokens: 0,
                audio_prompt_tokens: 0,
                image_prompt_tokens: 0,
                completion_tokens: 0,
                audio_completion_tokens: 0,
                reasoning_tokens: 0,
                ..TokenUsage::default()
            });
            let quote = calculate(&book, &calc, usage).map_err(AppError::from);
            match quote {
                Ok(quote) => {
                    let snapshot = super::upstream_cost::snapshot(
                        serde_json::to_value(&quote.snapshot).ok(),
                        channel.0,
                        channel.2,
                        quote.list_price,
                    );
                    let input = SettlementInput {
                        source_window: source_window.clone(),
                        dimensions: okapi_ledger::pg::UsageDimensions::new(
                            requested_model,
                            &upstream_model,
                            &format!("/v1{upstream_path}"),
                            &format!("/v1{upstream_path}"),
                        ),
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
                        list_price: quote.list_price,
                        upstream_cost: None,
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
                        balance_after: None,
                        event_type: "commit",
                        pool: reservation_pool,
                    };
                    failure.disarm();
                    if state
                        .settle_success(input)
                        .await
                        .inspect_err(|error| failure.settlement_failed(error))?
                    {
                        super::auth::record_settlement_counters(
                            state,
                            key.user_id,
                            key.member_user_id,
                            quote.amount.as_micros(),
                            usage.total_raw(),
                        )
                        .await;
                    }
                }
                Err(err) => {
                    failure.error(&err);
                    let _ = state
                        .ledger
                        .refund(key.user_id, key.key_id, request_id)
                        .await;
                    return Err(err);
                }
            }
            failure.disarm();
            let resp = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(resp_body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            Ok(with_request_id(resp, request_id))
        }
        Err((err, channel, failover)) => {
            failure.error(&err);
            let pool = match state
                .ledger
                .refund(key.user_id, key.key_id, request_id)
                .await
            {
                Ok(r) if !r.released.is_zero() => r.pool,
                Ok(_) => reservation_pool,
                Err(rerr) => {
                    tracing::error!(request_id = %request_id, error = %rerr, "embeddings 退款失败（悬置待清理）");
                    reservation_pool
                }
            };
            let input = SettlementInput {
                source_window: source_window.clone(),
                dimensions: okapi_ledger::pg::UsageDimensions::new(
                    requested_model,
                    channel.as_ref().map_or("", |c| c.2.as_str()),
                    &format!("/v1{upstream_path}"),
                    &if channel.as_ref().is_some_and(|c| !c.2.is_empty()) {
                        format!("/v1{upstream_path}")
                    } else {
                        String::new()
                    },
                ),
                request_id,
                log_type: 5,
                user_id: key.user_id,
                api_key_id: key.key_id,
                group_code: &key.group_code,
                model_name: &canonical,
                channel_id: channel.as_ref().map(|c| c.0),
                channel_key_id: channel.as_ref().map(|c| c.1),
                state: BillingState::Failed,
                usage: TokenUsage::default(),
                amount: Money::ZERO,
                original: Money::ZERO,
                discount: Money::ZERO,
                list_price: Money::ZERO,
                upstream_cost: None,
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
                pool,
            };
            state.settle_write(input).await;
            failure.disarm();
            Err(err)
        }
    }
}

type ForwardOk = (
    Bytes,
    u16,
    Option<TokenUsage>,
    (i64, i64, i64),
    Option<String>,
    i16,
    String,
);

fn validated_usage(
    usage: Option<okapi_api::UsageProbe>,
    est_prompt: u32,
) -> Result<Option<TokenUsage>, AppError> {
    usage
        .map(|probe| probe.with_estimates(est_prompt, 0))
        .transpose()
        .map_err(|_| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR))
}

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
    est_prompt: u32,
) -> Result<ForwardOk, (AppError, Option<(i64, i64, String)>, i16)> {
    let rows = okapi_store::channels::candidates_for_model(
        &state.pg,
        canonical,
        &key.pool_chain(),
        state.master_key.as_deref(),
    )
    .await
    .map_err(|e| (AppError::from(e), None, 0))?;
    let mut candidates: Vec<_> = super::scheduler::order_candidates(rows)
        .into_iter()
        .filter(|c| c.provider != "anthropic" && !super::dialect::chat_only(&c.provider))
        .collect();
    let margin_removed = state
        .retain_margin_ok(&key.group_code, &mut candidates)
        .await;
    if candidates.is_empty() {
        return Err((
            AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                super::state::no_candidates_code(margin_removed),
            ),
            None,
            0,
        ));
    }

    let mut failover: i16 = 0;
    let mut last: Option<(i64, i64, String)> = None;
    let mut last_error = AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR);
    for cand in candidates.into_iter().take(MAX_ATTEMPTS) {
        let upstream_model = cand.upstream_model(canonical).to_owned();
        let Ok(body_up) = rewrite_model(body, requested_model, &upstream_model) else {
            return Err((
                AppError::bad_request(),
                Some((cand.channel_id, cand.channel_key_id, String::new())),
                failover,
            ));
        };
        last = Some((cand.channel_id, cand.channel_key_id, upstream_model.clone()));

        match state
            .openai_json(&cand, &upstream_model, upstream_path, body_up)
            .await
        {
            Ok(resp) => {
                super::key_health::success(state, &cand).await;
                let usage = validated_usage(resp.usage, est_prompt)
                    .map_err(|error| (error, last.clone(), failover))?;
                return Ok((
                    resp.body,
                    resp.status,
                    usage,
                    (cand.channel_id, cand.channel_key_id, cand.cost_milli),
                    resp.upstream_request_id,
                    failover,
                    upstream_model,
                ));
            }
            Err(err) if err.retriable_before_first_token() => {
                last_error = if err.error_code() == codes::NO_AVAILABLE_CHANNEL {
                    super::account_control::attempt_error(&err)
                } else {
                    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                };
                let kind = super::chat::failure_kind_of(&err);
                super::key_health::failure(state, &cand, err.error_code(), kind).await;
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
    Err((last_error, last, failover))
}

fn elapsed_ms(started: Instant) -> i32 {
    i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::estimate_input_tokens;
    use serde_json::json;

    /// 405 汉字按 chars/4 只有 108：中文 embedding 输入必须按真实分词计数。
    #[test]
    fn cjk_input_is_not_undercounted_like_chars_div_four() {
        let text = "一二三四五".repeat(81);
        let est = estimate_input_tokens("text-embedding-3-small", &[&json!(text)]);
        assert!(
            est >= 300,
            "405 汉字的真实分词远超 chars/4 的 108，得到 {est}"
        );
        let en = estimate_input_tokens("text-embedding-3-small", &[&json!("word ".repeat(200))]);
        assert!(
            (150..=400).contains(&en),
            "200 词英文应在 150-400 tokens，得到 {en}"
        );
    }

    /// 预分词的 token id 数组按元素个数计，不喂给分词器；请求级常量与旧式 +3 对齐。
    #[test]
    fn raw_token_arrays_count_elements() {
        let est = estimate_input_tokens("text-embedding-3-small", &[&json!([101, 102, 103])]);
        assert_eq!(est, 6, "3 个 id + 请求级常量 3");
        let est = estimate_input_tokens("text-embedding-3-small", &[&json!("hi")]);
        assert_eq!(est, 4, "hi = 1 token + 请求级常量 3");
    }
}
