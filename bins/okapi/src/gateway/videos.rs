//! /v1/videos 异步任务面（IMPLEMENTATION §4.4 媒体计费，M3 顺延项）：
//! - `POST /v1/videos`：提交即 per_call × seconds 计费（乘数落 pricing_snapshot.media_units；
//!   时长无法本地验证，与 transcriptions 的 per_call 立场一致），上游提交失败或后续生成失败/取消退款；
//! - `GET /v1/videos/{id}`：任务轮询，按创建时的持久渠道映射回源（PG + Redis 缓存，user_id 隔离）；
//! - `GET /v1/videos/{id}/content`：成片流式透传（不整段缓冲）。
//!
//! 轮询/下载不计费；JSON 提交（multipart input_reference 列 backlog）。

use super::clients::detect_client_type;
use super::error::AppError;
use super::error::with_request_id;
use super::state::AppState;
use crate::gateway::extract::Path;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{BillingState, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_ledger::{LimitCaps, ReserveOutcome, SettlementInput};
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

fn parse_seconds(v: Option<&Value>) -> Result<u32, AppError> {
    let Some(v) = v else {
        return Ok(DEFAULT_SECONDS);
    };
    let n = match v {
        Value::String(s) => s.parse::<u32>().ok(),
        Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
        _ => None,
    };
    n.filter(|seconds| (1..=MAX_SECONDS).contains(seconds))
        .ok_or_else(|| AppError::bad_request().with_param("seconds"))
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
        list_price: Money::from_micros(quote.list_price.as_micros().saturating_mul(n)),
        snapshot,
    }
}

pub async fn create(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    match handle_create(&state, &headers, &body, request_id, started).await {
        Ok(resp) => with_request_id(resp, request_id),
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
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    let probe: VideosProbe = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let units = parse_seconds(probe.seconds.as_ref())?;
    // Forward the same quantity that was priced, including the default.
    let mut normalized: Value =
        serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    normalized["seconds"] = Value::String(units.to_string());
    let body = Bytes::from(serde_json::to_vec(&normalized).map_err(|_| AppError::bad_request())?);

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
    let quote = scale_quote(&calculate(&book, &calc, TokenUsage::default())?, units);
    if quote.snapshot.mode != "per_call" {
        return Err(AppError::bad_request().with_param("per_call_required"));
    }
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
                est: quote.amount,
                caps,
                est_tokens: 0,
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
        &probe.model,
        "/v1/videos",
        started,
        reservation_pool,
        source_window.as_deref(),
    );
    // —— 预扣已建立 ——
    let rows = okapi_store::channels::candidates_for_model(
        &state.pg,
        &canonical,
        &key.pool_chain(),
        state.master_key.as_deref(),
    )
    .await
    .map_err(AppError::from);
    let mut candidates: Vec<_> = match rows {
        Ok(rows) => super::scheduler::order_candidates(rows)
            .into_iter()
            // azure：Sora 走 `/openai/v1/video/generations/jobs` 另一套任务 API，本期不接
            .filter(|c| {
                !matches!(
                    c.provider.as_str(),
                    "anthropic" | "gemini" | "azure" | "bedrock" | "vertex"
                )
            })
            .collect(),
        Err(err) => {
            refund(state, &key, request_id, "videos").await;
            failure.error(&err);
            return Err(err);
        }
    };
    let margin_removed = state
        .retain_margin_ok(&key.group_code, &mut candidates)
        .await;
    if candidates.is_empty() {
        refund(state, &key, request_id, "videos").await;
        let error = AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            super::state::no_candidates_code(margin_removed),
        );
        failure.error(&error);
        return Err(error);
    }

    let mut failover: i16 = 0;
    let mut last_err: Option<AppError> = None;
    for cand in candidates.into_iter().take(MAX_ATTEMPTS) {
        failure.channel(&cand);
        let upstream_model = cand.upstream_model(&canonical).to_owned();
        let Ok(body_up) = rewrite_model(&body, &probe.model, &upstream_model) else {
            refund(state, &key, request_id, "videos").await;
            let error = AppError::bad_request();
            failure.error(&error);
            return Err(error);
        };
        let base = cand
            .api_base
            .clone()
            .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned());
        if let Some(trace) = super::diagnostics::Trace::current() {
            trace.media(&body_up, true);
        }
        match super::account_control::execute(
            state,
            &cand,
            &upstream_model,
            "/v1/videos",
            state.upstream.videos_create(
                &base,
                &cand.credential,
                body_up,
                &super::openai_dialect::outbound(&cand),
            ),
        )
        .await
        {
            Ok(resp) => {
                let task_id = serde_json::from_slice::<Value>(&resp.body)
                    .ok()
                    .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
                    .filter(|id| {
                        !id.is_empty()
                            && id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    });
                let Some(task_id) = task_id else {
                    refund(state, &key, request_id, "videos").await;
                    return Err(
                        AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                            .with_param("task_id_missing"),
                    );
                };
                super::key_health::success(state, &cand).await;
                state
                    .sched
                    .video_task_set(key.user_id, &task_id, cand.channel_key_id)
                    .await;
                commit_and_record(
                    state,
                    &key,
                    &canonical,
                    &probe.model,
                    &quote,
                    &task_id,
                    units,
                    request_id,
                    started,
                    &cand,
                    failover,
                    headers,
                    reservation_pool,
                    source_window.as_deref(),
                    &mut failure,
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
            Err(err) if err.retriable_before_first_token() => {
                super::key_health::failure(
                    state,
                    &cand,
                    err.error_code(),
                    super::chat::failure_kind_of(&err),
                )
                .await;
                failover = failover.saturating_add(1);
                last_err = Some(super::account_control::attempt_error(&err));
            }
            Err(err) => {
                last_err = Some(super::account_control::attempt_error(&err));
                break;
            }
        }
    }

    refund(state, &key, request_id, "videos").await;
    let error =
        last_err.unwrap_or_else(|| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR));
    failure.error(&error);
    Err(error)
}

/// 任务轮询：映射回源，JSON 透传（不计费）。
pub async fn get_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let request_id = Uuid::new_v4();
    match relay_task(&state, &headers, &task_id, false).await {
        Ok(resp) => with_request_id(resp, request_id),
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
        Ok(resp) => with_request_id(resp, request_id),
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

async fn relay_task(
    state: &AppState,
    headers: &HeaderMap,
    task_id: &str,
    content: bool,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    // 轮询与下载都拿渠道凭证打上游，不计费也要按分组窗限速（§11.32）。
    // 不过 check_member_limit——花超的成员仍得取回已经付过费的视频
    super::auth::check_group_rate(state, &key).await?;
    // 键含 user_id：他人任务/过期/未知一律 404（不泄露存在性）
    let channel_key_id = sqlx::query_scalar!(
        "SELECT channel_key_id FROM video_tasks WHERE user_id=$1 AND task_id=$2",
        key.user_id,
        task_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let channel_key_id = match channel_key_id {
        Some(id) => Some(id),
        None => state.sched.video_task_get(key.user_id, task_id).await,
    };
    let Some(channel_key_id) = channel_key_id else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND).with_param("task"));
    };
    let Some(ch) = okapi_store::channels::channel_key_ref(
        &state.pg,
        channel_key_id,
        state.master_key.as_deref(),
    )
    .await?
    else {
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
        let outbound = okapi_providers::Outbound {
            proxy_url: ch.proxy_url.clone(),
            extra_headers: ch.extra_headers.clone(),
            ..Default::default()
        };
        let resp = tokio::time::timeout(
            std::time::Duration::from_mins(2),
            download_response(state, &base, &path, &ch.credential, &outbound),
        )
        .await
        .map_err(|_| AppError::new(StatusCode::GATEWAY_TIMEOUT, codes::UPSTREAM_TIMEOUT))??;
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
            .get_json(
                &base,
                &path,
                &ch.credential,
                &okapi_providers::Outbound {
                    proxy_url: ch.proxy_url.clone(),
                    extra_headers: ch.extra_headers.clone(),
                    ..Default::default()
                },
            )
            .await
            .map_err(|_| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR))?;
        observe_task(state, key.user_id, task_id, &resp.body).await?;
        Ok(Response::builder()
            .status(resp.status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(resp.body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
    }
}

async fn download_response(
    state: &AppState,
    base: &str,
    path: &str,
    credential: &str,
    outbound: &okapi_providers::Outbound,
) -> Result<reqwest::Response, AppError> {
    let invalid =
        |reason| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR).with_param(reason);
    let mut url = reqwest::Url::parse(&format!("{}{path}", base.trim_end_matches('/')))
        .map_err(|_| invalid("video_download_url"))?;
    let upstream_origin = url.origin();
    let public_outbound = okapi_providers::Outbound {
        proxy_url: outbound.proxy_url.clone(),
        extra_headers: Vec::new(),
        ..Default::default()
    };
    for hop in 0..=5 {
        let same_origin = url.origin() == upstream_origin;
        let response = state
            .upstream
            .get_stream_url(
                url.as_str(),
                same_origin.then_some(credential),
                if same_origin {
                    outbound
                } else {
                    &public_outbound
                },
            )
            .await
            .map_err(|_| invalid("video_download_request"))?;
        if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        if hop == 5 {
            return Err(invalid("video_download_redirect_limit"));
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid("video_download_redirect_location"))?;
        let next = url
            .join(location)
            .map_err(|_| invalid("video_download_redirect_location"))?;
        if url.scheme() == "https" && next.scheme() != "https" {
            return Err(invalid("video_download_redirect_scheme"));
        }
        crate::console::ssrf::validate_url(&state.pg, next.as_str())
            .await
            .map_err(|_| invalid("video_download_redirect_target"))?;
        url = next;
    }
    Err(invalid("video_download_redirect_limit"))
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
    requested_model: &str,
    quote: &Quote,
    task_id: &str,
    _units: u32,
    request_id: Uuid,
    started: Instant,
    cand: &okapi_store::ChannelCandidate,
    failover: i16,
    headers: &HeaderMap,
    reservation_pool: okapi_ledger::Pool,
    source_window: Option<&str>,
    failure: &mut super::failure::Guard,
) -> Result<(), AppError> {
    let input = SettlementInput {
        source_window: source_window.map(str::to_owned),
        dimensions: okapi_ledger::pg::UsageDimensions::new(
            requested_model,
            cand.upstream_model(canonical),
            "/v1/videos",
            "/v1/videos",
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
        pricing_epoch: Some(quote.snapshot.epoch),
        pricing_snapshot: super::upstream_cost::snapshot(
            serde_json::to_value(&quote.snapshot).ok(),
            cand.channel_id,
            cand.cost_milli,
            quote.list_price,
        ),
        latency_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
        ttft_ms: None,
        is_stream: false,
        retry_count: 0,
        failover_count: failover,
        upstream_status: Some(200),
        error_code: None,
        upstream_request_id: Some(task_id),
        node: state.node.as_ref(),
        sticky_layer: 0,
        client_type: detect_client_type(headers),
        client_ip: None,
        delta_micro: quote.amount.as_micros().saturating_neg(),
        balance_after: None,
        event_type: "commit",
        pool: reservation_pool,
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
        0,
    )
    .await;
    Ok(())
}

/// Both portal polling and worker polling converge on the same idempotent refund.
async fn observe_task(
    state: &AppState,
    user_id: i64,
    task_id: &str,
    body: &[u8],
) -> Result<(), AppError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR))?;
    let status = value.get("status").and_then(Value::as_str).unwrap_or("");
    if !matches!(status, "failed" | "cancelled" | "canceled" | "completed") {
        return Ok(());
    }
    if status == "completed" {
        sqlx::query!("UPDATE video_tasks SET state='completed',updated_at=now() WHERE user_id=$1 AND task_id=$2 AND state='pending'",user_id,task_id).execute(&state.pg).await.map_err(okapi_store::StoreError::from)?;
        return Ok(());
    }
    let request_id=sqlx::query_scalar!("UPDATE video_tasks SET state='refund_pending',updated_at=now() WHERE user_id=$1 AND task_id=$2 AND state IN ('pending','refund_pending') RETURNING request_id",user_id,task_id).fetch_optional(&state.pg).await.map_err(okapi_store::StoreError::from)?;
    let Some(request_id) = request_id else {
        return Ok(());
    };
    okapi_ledger::operations::refund(
        &state.pg,
        &state.ledger,
        request_id,
        "video_generation_failed",
        "system:worker",
    )
    .await?;
    sqlx::query!("UPDATE video_tasks SET state='refunded',updated_at=now() WHERE user_id=$1 AND task_id=$2 AND state='refund_pending'",user_id,task_id).execute(&state.pg).await.map_err(okapi_store::StoreError::from)?;
    Ok(())
}

/// 生成期限：过了仍未完成的任务按失败退款。
const TASK_EXPIRY: chrono::TimeDelta = chrono::TimeDelta::hours(24);
/// 一直查不到上游状态时的放弃期限。
const POLL_GIVE_UP: chrono::TimeDelta = chrono::TimeDelta::hours(72);

fn is_terminal(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|status| {
            matches!(
                status.as_str(),
                "failed" | "cancelled" | "canceled" | "completed"
            )
        })
}

pub async fn poll_pending(state: &AppState) -> anyhow::Result<()> {
    use futures::StreamExt as _;
    let tasks = sqlx::query!("UPDATE video_tasks SET next_poll_at=now()+interval '1 minute' WHERE (user_id,task_id) IN (SELECT user_id,task_id FROM video_tasks WHERE state IN ('pending','refund_pending') AND next_poll_at<=now() ORDER BY next_poll_at LIMIT 100 FOR UPDATE SKIP LOCKED) RETURNING user_id,task_id,channel_key_id,created_at,state").fetch_all(&state.pg).await?;
    futures::stream::iter(tasks)
        .map(|task| async move {
            let (user_id, task_id, channel_key_id) =
                (task.user_id, task.task_id, task.channel_key_id);
            let result = async {
                const FAILED: &[u8] = br#"{"status":"failed"}"#;
                if task.state == "refund_pending" {
                    return observe_task(state, user_id, &task_id, FAILED).await;
                }
                let age = chrono::Utc::now().signed_duration_since(task.created_at);
                let polled = async {
                    let ch = okapi_store::channels::channel_key_ref(
                        &state.pg,
                        channel_key_id,
                        state.master_key.as_deref(),
                    )
                    .await?
                    .ok_or_else(AppError::internal)?;
                    state
                        .upstream
                        .get_json(
                            ch.api_base.as_deref().unwrap_or(DEFAULT_OPENAI_BASE),
                            &format!("/videos/{task_id}"),
                            &ch.credential,
                            &okapi_providers::Outbound {
                                proxy_url: ch.proxy_url,
                                extra_headers: ch.extra_headers,
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(|_| AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR))
                }
                .await;
                match polled {
                    // 过了生成期限仍未出结果的按失败退款；上游已完成的照常收费
                    Ok(response) if age >= TASK_EXPIRY && !is_terminal(&response.body) => {
                        observe_task(state, user_id, &task_id, FAILED).await
                    }
                    Ok(response) => observe_task(state, user_id, &task_id, &response.body).await,
                    // 查不到状态不等于失败：用户可能已取回成片。宽限到放弃期限才退款
                    Err(_) if age >= POLL_GIVE_UP => {
                        observe_task(state, user_id, &task_id, FAILED).await
                    }
                    Err(error) => Err(error),
                }
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(user_id,%task_id,code=%error.code,"video task polling deferred");
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
    Ok(())
}

#[cfg(test)]
mod duration_tests {
    use super::*;
    #[test]
    fn duration_is_exact_or_rejected() {
        assert_eq!(parse_seconds(None).unwrap(), 4);
        for value in [serde_json::json!("12"), serde_json::json!(12)] {
            assert_eq!(parse_seconds(Some(&value)).unwrap(), 12);
        }
        for value in [
            serde_json::json!(120),
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("invalid"),
            Value::Null,
        ] {
            assert!(parse_seconds(Some(&value)).is_err());
        }
    }
}
