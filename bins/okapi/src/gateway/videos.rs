//! /v1/videos 异步任务面（IMPLEMENTATION §4.4 媒体计费，M3 顺延项）：
//! - `POST /v1/videos`：提交即 per_call × seconds 计费（乘数落 pricing_snapshot.media_units；
//!   时长无法本地验证，与 transcriptions 的 per_call 立场一致），上游失败退款；
//! - `GET /v1/videos/{id}`：任务轮询，按创建时的渠道映射回源（Redis 48h，键含 user_id 隔离）；
//! - `GET /v1/videos/{id}/content`：成片流式透传（不整段缓冲）。
//!
//! 轮询/下载不计费；JSON 提交（multipart input_reference 列 backlog）。

use super::clients::detect_client_type;
use super::error::AppError;
use super::state::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{BillingState, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_ledger::{CommitOutcome, LimitCaps, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, Quote, RatioFp, calculate};
use okapi_providers::rewrite_model;
use serde::Deserialize;
use serde_json::Value;
use std::time::Instant;
use uuid::Uuid;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const MAX_ATTEMPTS: usize = 3;
const DEFAULT_SECONDS: u32 = 4;
const MAX_SECONDS: u32 = 60;

#[derive(Deserialize)]
struct VideosProbe {
    model: String,
    /// OpenAI 形状为字符串（"4"/"8"/"12"），兼容数字。
    #[serde(default)]
    seconds: Option<Value>,
}

fn parse_seconds(v: Option<&Value>) -> u32 {
    let n = match v {
        Some(Value::String(s)) => s.parse::<u32>().ok(),
        Some(Value::Number(n)) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
        _ => None,
    };
    n.unwrap_or(DEFAULT_SECONDS).clamp(1, MAX_SECONDS)
}

/// per_call 报价 × 秒数（整数饱和乘，乘数记入快照供账单解释）。
fn scale_quote(quote: &Quote, units: u32) -> Quote {
    let n = i64::from(units);
    let mut snapshot = quote.snapshot.clone();
    snapshot.media_units = Some(units);
    Quote {
        amount: Money::from_micros(quote.amount.as_micros().saturating_mul(n)),
        original: Money::from_micros(quote.original.as_micros().saturating_mul(n)),
        discount: Money::from_micros(quote.discount.as_micros().saturating_mul(n)),
        snapshot,
    }
}

pub async fn create(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    match handle_create(&state, &headers, &body, request_id, started).await {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

// 时序与 images 主链一致（鉴权→估价→预扣→failover→commit）
#[allow(clippy::too_many_lines)]
async fn handle_create(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: Uuid,
    started: Instant,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate(state, headers).await?;
    let probe: VideosProbe = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let units = parse_seconds(probe.seconds.as_ref());

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
    let rows =
        okapi_store::channels::candidates_for_model(&state.pg, &canonical, key.pool_code.as_deref())
            .await
            .map_err(AppError::from);
    let candidates: Vec<_> = match rows {
        Ok(rows) => super::scheduler::order_candidates(rows)
            .into_iter()
            .filter(|c| c.provider != "anthropic" && c.provider != "gemini")
            .collect(),
        Err(err) => {
            refund(state, &key, request_id, "videos").await;
            return Err(err);
        }
    };
    if candidates.is_empty() {
        refund(state, &key, request_id, "videos").await;
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
            refund(state, &key, request_id, "videos").await;
            return Err(AppError::bad_request());
        };
        let base = cand
            .api_base
            .clone()
            .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned());
        match state
            .upstream
            .videos_create(&base, &cand.credential, body_up)
            .await
        {
            Ok(resp) => {
                // 任务→渠道映射：轮询/下载回源锚点（缺 id 只降级为不可轮询，不阻塞返回）
                if let Some(task_id) = serde_json::from_slice::<Value>(&resp.body)
                    .ok()
                    .as_ref()
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                {
                    state
                        .sched
                        .video_task_set(key.user_id, task_id, cand.channel_key_id)
                        .await;
                } else {
                    tracing::warn!(request_id = %request_id, "videos 上游响应缺 id，任务不可轮询");
                }
                commit_and_record(
                    state, &key, &canonical, &quote, units, request_id, started, &cand, failover,
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

    refund(state, &key, request_id, "videos").await;
    Err(last_err.unwrap_or_else(|| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)))
}

/// 任务轮询：映射回源，JSON 透传（不计费）。
pub async fn get_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let request_id = Uuid::new_v4();
    match relay_task(&state, &headers, &task_id, false).await {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// 成片下载：映射回源，字节流透传（不计费）。
pub async fn get_content(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let request_id = Uuid::new_v4();
    match relay_task(&state, &headers, &task_id, true).await {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

async fn relay_task(
    state: &AppState,
    headers: &HeaderMap,
    task_id: &str,
    content: bool,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate(state, headers).await?;
    // 键含 user_id：他人任务/过期/未知一律 404（不泄露存在性）
    let Some(channel_key_id) = state.sched.video_task_get(key.user_id, task_id).await else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND).with_param("task"));
    };
    let Some(ch) = okapi_store::channels::channel_key_ref(&state.pg, channel_key_id).await? else {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::NO_AVAILABLE_CHANNEL,
        ));
    };
    let base = ch
        .api_base
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned());
    // task_id 来源于上游返回值，仍按路径段白名单字符校验防拼接逃逸
    if !task_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AppError::bad_request());
    }

    if content {
        let path = format!("/videos/{task_id}/content");
        let resp = state
            .upstream
            .get_stream(&base, &path, &ch.credential)
            .await
            .map_err(|_| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR))?;
        let status =
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let body = Body::from_stream(resp.bytes_stream());
        Ok(Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
    } else {
        let path = format!("/videos/{task_id}");
        let resp = state
            .upstream
            .get_json(&base, &path, &ch.credential)
            .await
            .map_err(|_| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR))?;
        Ok(Response::builder()
            .status(resp.status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(resp.body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
    }
}

async fn refund(state: &AppState, key: &okapi_store::AuthedKey, request_id: Uuid, tag: &str) {
    if let Err(err) = state
        .ledger
        .refund(key.user_id, key.key_id, request_id)
        .await
    {
        tracing::error!(request_id = %request_id, error = %err, "{tag} 退款失败（悬置待清理）");
    }
}

#[allow(clippy::too_many_arguments)]
async fn commit_and_record(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    canonical: &str,
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
            tracing::warn!(request_id = %request_id, "videos 重复结算竞争，跳过");
        }
        Err(err) => {
            tracing::error!(request_id = %request_id, error = %err, "videos Redis 结算失败（悬置待清理）");
        }
    }
}
