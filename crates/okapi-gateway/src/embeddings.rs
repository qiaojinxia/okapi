//! /v1/embeddings：鉴权 → 估价预扣（仅 prompt）→ 候选 failover → 结算。
//! 非流式单跳，复用 chat 同款调度语义（并发槽/状态机/退款兜底），
//! anthropic 渠道无 embeddings 端点，候选层跳过。

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
use okapi_kernel::error::with_request_id;
use okapi_ledger::SettlementInput;
use okapi_pricing::calculate;
use okapi_providers::registry::MediaSurface;
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
/// 数字元素是预分词的 token id 序列，个数即约 token 数。对象逐个取值往下数：rerank 的标准形状
/// `documents: [{"text": "…"}]`（Jina / Cohere）以前按 0 计，上游不返 usage 时文档文本整段白送。
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
            serde_json::Value::Object(fields) => {
                for value in fields.values() {
                    collect(value, texts, raw_tokens);
                }
            }
            serde_json::Value::Bool(_) | serde_json::Value::Null => {}
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
    match handle(
        &state,
        &headers,
        &body,
        request_id,
        started,
        &probe.model,
        "/embeddings",
        || estimate_input_tokens(&probe.model, &[&probe.input]),
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
    let query = serde_json::Value::String(probe.query.clone());
    match handle(
        &state,
        &headers,
        &body,
        request_id,
        started,
        &probe.model,
        "/rerank",
        || estimate_input_tokens(&probe.model, &[&query, &probe.documents]),
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
    estimate: impl FnOnce() -> u32,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    // 分词放在鉴权之后：匿名请求不该让网关替它跑整段 BPE（与 images 先鉴权再碰请求体同一纪律）
    let est_prompt = estimate();

    let meta = super::candidates::resolve_model_cached(state, requested_model).await?;
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
    let calc = super::admission::calc_context(&key, canonical.as_str(), rules_in, now, None);

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
    super::admission::check_model_rpm(state, &key, &canonical).await?;

    let super::admission::Reserved {
        pool: reservation_pool,
        source_window,
    } = super::admission::reserve(
        state,
        &key,
        request_id,
        est_quote.amount,
        u64::from(est_prompt),
        now,
    )
    .await?;

    let origin = Origin::of(headers);
    let payer = Payer::new(
        state,
        &key,
        request_id,
        &origin,
        reservation_pool,
        source_window.as_deref(),
    );
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
        &origin,
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
                        log_type: 2,
                        channel_id: Some(channel.0),
                        channel_key_id: Some(channel.1),
                        state: BillingState::Committed,
                        usage,
                        amount: quote.amount,
                        original: quote.original,
                        discount: quote.discount,
                        list_price: quote.list_price,
                        pricing_epoch: Some(book.epoch()),
                        pricing_snapshot: snapshot,
                        latency_ms: elapsed_ms(started),
                        failover_count: failover,
                        upstream_status: Some(200),
                        upstream_request_id: upstream_request_id.as_deref(),
                        sticky_layer: 3,
                        delta_micro: quote.amount.as_micros().saturating_neg(),
                        event_type: "commit",
                        ..payer.settlement(
                            &canonical,
                            okapi_ledger::pg::UsageDimensions::new(
                                requested_model,
                                &upstream_model,
                                &format!("/v1{upstream_path}"),
                                &format!("/v1{upstream_path}"),
                            ),
                        )
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
                channel_id: channel.as_ref().map(|c| c.0),
                channel_key_id: channel.as_ref().map(|c| c.1),
                pricing_epoch: Some(book.epoch()),
                latency_ms: elapsed_ms(started),
                failover_count: failover,
                error_code: Some(err.code.as_str()),
                pool,
                ..payer.settlement(
                    &canonical,
                    okapi_ledger::pg::UsageDimensions::new(
                        requested_model,
                        channel.as_ref().map_or("", |c| c.2.as_str()),
                        &format!("/v1{upstream_path}"),
                        &if channel.as_ref().is_some_and(|c| !c.2.is_empty()) {
                            format!("/v1{upstream_path}")
                        } else {
                            String::new()
                        },
                    ),
                )
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
/// 上游回了 2xx 却不是 JSON 对象（站点的网页前端 / 网关页）：不是一次成功的调用，
/// 不转发、不计费，当作这个候选的瞬态失败换下一个。None = 是 JSON，照常处理。
async fn non_json_failure(
    state: &AppState,
    cand: &okapi_store::ChannelCandidate,
    request_id: Uuid,
    body: &[u8],
) -> Option<AppError> {
    let kind = super::upstream_failure::non_json_kind(body)?;
    super::key_health::failure(
        state,
        cand,
        codes::UPSTREAM_ERROR,
        okapi_store::channels::KeyFailure::Transient,
    )
    .await;
    tracing::warn!(request_id = %request_id, channel_key = cand.channel_key_id,
        kind, bytes = body.len(), "embeddings 上游回了非 JSON，failover 下一候选");
    Some(AppError::new(
        StatusCode::BAD_GATEWAY,
        codes::UPSTREAM_ERROR,
    ))
}

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
    // rerank 与 embeddings 是同一条 OpenAI 形态的 JSON 中继，按同一面选渠道
    let candidates = super::candidates::media(state, key, canonical, MediaSurface::Embeddings)
        .await
        .map_err(|e| (e, None, 0))?;

    let mut failover: i16 = 0;
    let mut last: Option<(i64, i64, String)> = None;
    let mut last_error = AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR);
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
                if let Some(error) = non_json_failure(state, &cand, request_id, &resp.body).await {
                    last_error = error;
                    failover = failover.saturating_add(1);
                    continue;
                }
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
                let kind = super::upstream_failure::failure_kind_of(&err);
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
    if attempted == 0 {
        last_error = AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL);
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

    #[test]
    fn rerank_object_documents_count_their_text() {
        let text = "word ".repeat(200);
        let plain = estimate_input_tokens("rerank-x", &[&json!("q"), &json!([text.clone()])]);
        let objects = estimate_input_tokens("rerank-x", &[&json!("q"), &json!([{"text": text}])]);
        assert_eq!(objects, plain, "{{\"text\": …}} 与字符串文档同样计数");
        assert!(objects > 100);
    }
}
