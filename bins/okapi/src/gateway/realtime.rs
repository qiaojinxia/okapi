//! OpenAI Realtime API 桥接（IMPLEMENTATION §4.4 M4）：
//! `GET /v1/realtime?model=` WS 升级 ↔ 上游 wss 双向泵。
//! 计费：连接时按模型 max_output 预扣，会话内逐 `response.done` 累计 usage
//! （text+audio 合并按模型倍率；audio 独立倍率列 backlog），断开按累计 commit，
//! 无产出全额退款。治理（§14.4）：per-key WS 并发上限（Redis 计数 + 兜底 TTL）、
//! 首消息 30s、空闲 5min。

use super::error::AppError;
use super::state::AppState;
use axum::extract::ws::{Message as AxumMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use okapi_api::codes;
use okapi_domain::{BillingState, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_ledger::{CommitOutcome, LimitCaps, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, RatioFp, calculate};
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message as TungMsg;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use uuid::Uuid;

const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_mins(5);
const DEFAULT_COMPLETION_CAP: u32 = 4096;

#[derive(Deserialize)]
pub struct RealtimeQuery {
    pub model: String,
}

/// Bearer 头缺省时兼容 OpenAI 客户端子协议 `openai-insecure-api-key.<key>`。
fn token_from_subprotocol(headers: &HeaderMap) -> Option<String> {
    headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())?
        .split(',')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("openai-insecure-api-key."))
        .map(str::to_owned)
}

pub async fn realtime(
    State(state): State<AppState>,
    Query(q): Query<RealtimeQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let request_id = Uuid::new_v4();
    // 鉴权在升级前完成：拒绝直接走 HTTP 错误（客户端拿到 401/403 而非握手失败裸断）
    let key = if headers.contains_key(axum::http::header::AUTHORIZATION) {
        super::auth::authenticate(&state, &headers).await
    } else if let Some(token) = token_from_subprotocol(&headers) {
        let mut synth = HeaderMap::new();
        if let Ok(v) = axum::http::HeaderValue::from_str(&format!("Bearer {token}")) {
            synth.insert(axum::http::header::AUTHORIZATION, v);
        }
        super::auth::authenticate(&state, &synth).await
    } else {
        Err(AppError::unauthorized(codes::INVALID_API_KEY))
    };
    let key = match key {
        Ok(k) => k,
        Err(err) => return err.into_response_with(Some(request_id)),
    };

    match prepare(&state, &key, &q.model, request_id).await {
        Ok(prep) => ws
            .protocols(["realtime"])
            .on_upgrade(move |socket| async move {
                bridge_session(state, socket, prep).await;
            }),
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

struct Prep {
    key: std::sync::Arc<okapi_store::AuthedKey>,
    request_id: Uuid,
    canonical: String,
    upstream_url: String,
    credential: String,
    channel: (i64, i64),
    calc: CalcContext,
    started: Instant,
}

/// 升级前置链：模型解析 → 估价 → 限连占槽 → 预扣 → 候选。失败即 HTTP 错误（不升级）。
// 鉴权→估价→占槽→预扣→选路的线性前置链，拆分损害时序可读性
#[allow(clippy::too_many_lines)]
async fn prepare(
    state: &AppState,
    key: &std::sync::Arc<okapi_store::AuthedKey>,
    requested_model: &str,
    request_id: Uuid,
) -> Result<Prep, AppError> {
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
    super::auth::check_member_limit(state, key).await?;

    let book = state.pricebook.load();
    let rules_in = super::rule_inputs::collect(state, &book, key.user_id).await;
    let now = chrono::Utc::now();
    let calc = CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(canonical.as_str()),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: rules_in.monthly_tokens,
        local_minute_of_day: u16::try_from((now.timestamp().div_euclid(60)).rem_euclid(1440))
            .unwrap_or(0),
        now_unix: now.timestamp(),
        surge_active: rules_in.surge_active,
        service_tier: None,
    };
    let cap = meta
        .max_output
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(DEFAULT_COMPLETION_CAP);
    let est_usage = TokenUsage {
        prompt_tokens: cap,
        cached_tokens: 0,
        cache_write_tokens: 0,
        audio_prompt_tokens: 0,
        image_prompt_tokens: 0,
        completion_tokens: cap,
        audio_completion_tokens: 0,
        reasoning_tokens: 0,
    };
    let est = calculate(&book, &calc, est_usage)?;

    // §14.4：per-key WS 连接租约（settings.realtime_max_conns_per_key 缺省 4）
    let limit = state
        .setting_cached("realtime_max_conns_per_key")
        .await
        .as_ref()
        .as_ref()
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(4);
    let conn_id = request_id.to_string();
    if !state
        .sched
        .ws_lease_acquire(key.key_id, &conn_id, limit)
        .await
    {
        return Err(
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("realtime_conns"),
        );
    }

    let caps = {
        let c = |v: Option<i32>| v.map_or(0, i64::from);
        LimitCaps {
            rpm: c(key.rpm_limit),
            tpm: c(key.tpm_limit),
            rpd: c(key.rpd_limit),
            concurrency: c(key.max_concurrency),
        }
    };
    let reserve = state
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: key.user_id,
                api_key_id: key.key_id,
                request_id,
                est: est.amount,
                caps,
                est_tokens: u64::from(cap) * 2,
            },
            now,
        )
        .await;
    match reserve {
        Ok(ReserveOutcome::Reserved { .. }) => {}
        Ok(ReserveOutcome::Insufficient { .. }) => {
            state.sched.ws_lease_release(key.key_id, &conn_id).await;
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                codes::INSUFFICIENT_QUOTA,
            ));
        }
        Ok(ReserveOutcome::RateLimited { which }) => {
            state.sched.ws_lease_release(key.key_id, &conn_id).await;
            return Err(
                AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(which),
            );
        }
        Err(err) => {
            state.sched.ws_lease_release(key.key_id, &conn_id).await;
            return Err(err.into());
        }
    }

    // 候选：openai 协议渠道（anthropic/gemini 无 Realtime 面；能力显式 false 排除）
    let rows =
        okapi_store::channels::candidates_for_model(&state.pg, &canonical, &key.visibility_groups)
            .await;
    let cand = match rows {
        Ok(rows) => super::scheduler::order_candidates(rows)
            .into_iter()
            .find(|c| {
                c.provider != "anthropic"
                    && c.provider != "gemini"
                    && c.capabilities.get("realtime").and_then(Value::as_bool) != Some(false)
            }),
        Err(err) => {
            release_reservation_and_slot(state, key, request_id).await;
            return Err(err.into());
        }
    };
    let Some(cand) = cand else {
        release_reservation_and_slot(state, key, request_id).await;
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::NO_AVAILABLE_CHANNEL,
        ));
    };
    let upstream_model = cand.upstream_model(&canonical).to_owned();
    let base = cand
        .api_base
        .clone()
        .unwrap_or_else(|| "https://api.openai.com/v1".to_owned());
    let ws_base = base
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let upstream_url = format!(
        "{}/realtime?model={upstream_model}",
        ws_base.trim_end_matches('/')
    );

    Ok(Prep {
        key: std::sync::Arc::clone(key),
        request_id,
        canonical,
        upstream_url,
        credential: cand.credential.clone(),
        channel: (cand.channel_id, cand.channel_key_id),
        calc,
        started: Instant::now(),
    })
}

async fn release_reservation_and_slot(state: &AppState, key: &okapi_store::AuthedKey, id: Uuid) {
    if let Err(err) = state.ledger.refund(key.user_id, key.key_id, id).await {
        tracing::error!(request_id = %id, error = %err, "realtime 退款失败（悬置待 sweep）");
    }
    state
        .sched
        .ws_lease_release(key.key_id, &id.to_string())
        .await;
}

/// 会话主体：连上游 → 双向泵 → 断开结算（治理超时内置于泵循环）。
// 双向泵与结算的线性会话时序
#[allow(clippy::too_many_lines)]
async fn bridge_session(state: AppState, client: WebSocket, prep: Prep) {
    let request_id = prep.request_id;
    let mut req = match prep.upstream_url.clone().into_client_request() {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(request_id = %request_id, error = %err, "realtime 上游 URL 非法");
            fail_session(&state, &prep, client, codes::UPSTREAM_ERROR).await;
            return;
        }
    };
    if let Ok(v) = format!("Bearer {}", prep.credential).parse() {
        req.headers_mut().insert("authorization", v);
    }
    let upstream =
        tokio::time::timeout(FIRST_MESSAGE_TIMEOUT, tokio_tungstenite::connect_async(req)).await;
    let (upstream_ws, _) = match upstream {
        Ok(Ok(pair)) => pair,
        Ok(Err(err)) => {
            tracing::warn!(request_id = %request_id, error = %err, "realtime 上游连接失败");
            fail_session(&state, &prep, client, codes::UPSTREAM_ERROR).await;
            return;
        }
        Err(_) => {
            fail_session(&state, &prep, client, codes::UPSTREAM_TIMEOUT).await;
            return;
        }
    };

    let (mut up_tx, mut up_rx) = upstream_ws.split();
    let (mut cl_tx, mut cl_rx) = client.split();
    let mut usage = TokenUsage::default();
    let mut responses: u32 = 0;
    let mut awaiting_first = true;
    let conn_id = request_id.to_string();
    let mut renew = tokio::time::interval(Duration::from_secs(20));
    renew.tick().await; // 首个 tick 立即完成，跳过

    // 双向泵：任一侧关闭/出错即收尾；超时窗口 = 首消息 30s，其后任意方向静默 5min；
    // 每 20s 续租连接租约（§14.4）
    loop {
        let window = if awaiting_first {
            FIRST_MESSAGE_TIMEOUT
        } else {
            IDLE_TIMEOUT
        };
        tokio::select! {
            () = tokio::time::sleep(window) => {
                tracing::info!(request_id = %request_id, awaiting_first, "realtime 超时，关闭会话");
                break;
            }
            _ = renew.tick() => {
                state.sched.ws_lease_renew(prep.key.key_id, &conn_id).await;
            }
            msg = cl_rx.next() => {
                match msg {
                    Some(Ok(m)) => {
                        awaiting_first = false;
                        let forward = match m {
                            AxumMsg::Text(t) => TungMsg::text(t.to_string()),
                            AxumMsg::Binary(b) => TungMsg::binary(b),
                            AxumMsg::Ping(p) => TungMsg::Ping(p),
                            AxumMsg::Pong(p) => TungMsg::Pong(p),
                            AxumMsg::Close(_) => break,
                        };
                        if up_tx.send(forward).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            msg = up_rx.next() => {
                match msg {
                    Some(Ok(m)) => {
                        if let TungMsg::Text(text) = &m {
                            accumulate_usage(text, &mut usage, &mut responses);
                        }
                        let forward = match m {
                            TungMsg::Text(t) => AxumMsg::Text(t.as_str().into()),
                            TungMsg::Binary(b) => AxumMsg::Binary(b),
                            TungMsg::Ping(p) => AxumMsg::Ping(p),
                            TungMsg::Pong(p) => AxumMsg::Pong(p),
                            TungMsg::Close(_) => break,
                            TungMsg::Frame(_) => continue,
                        };
                        if cl_tx.send(forward).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }
    let _ = cl_tx.close().await;
    let _ = up_tx.close().await;

    settle_session(&state, &prep, usage, responses).await;
    state
        .sched
        .ws_lease_release(prep.key.key_id, &conn_id)
        .await;
}

/// `response.done` → usage 累计（input/output 已含 audio tokens，text+audio 合并计费；
/// 缓存文本进 cached 打折口径）。
fn accumulate_usage(text: &str, usage: &mut TokenUsage, responses: &mut u32) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if v.get("type").and_then(Value::as_str) != Some("response.done") {
        return;
    }
    let Some(u) = v.pointer("/response/usage") else {
        return;
    };
    let get = |k: &str| {
        u.get(k)
            .and_then(Value::as_u64)
            .and_then(|x| u32::try_from(x).ok())
            .unwrap_or(0)
    };
    let cached = u
        .pointer("/input_token_details/cached_tokens")
        .and_then(Value::as_u64)
        .and_then(|x| u32::try_from(x).ok())
        .unwrap_or(0);
    usage.prompt_tokens = usage.prompt_tokens.saturating_add(get("input_tokens"));
    usage.cached_tokens = usage.cached_tokens.saturating_add(cached);
    usage.completion_tokens = usage.completion_tokens.saturating_add(get("output_tokens"));
    *responses = responses.saturating_add(1);
}

/// 升级后上游失败：客户端收一条 error 事件（error_code 语义，前端语言包渲染），
/// 退款 + 释放槽 + 失败留痕。
async fn fail_session(state: &AppState, prep: &Prep, mut client: WebSocket, code: &str) {
    let event = serde_json::json!({
        "type": "error",
        "error": {"type": "upstream", "code": code},
    });
    let _ = client.send(AxumMsg::Text(event.to_string().into())).await;
    let _ = client.close().await;
    release_reservation_and_slot(state, &prep.key, prep.request_id).await;
    record_failure(state, prep, TokenUsage::default(), Some(code)).await;
}

/// 断开结算：有产出按累计 usage commit；零产出全额退款留痕。
async fn settle_session(state: &AppState, prep: &Prep, usage: TokenUsage, responses: u32) {
    if responses == 0 {
        if let Err(err) = state
            .ledger
            .refund(prep.key.user_id, prep.key.key_id, prep.request_id)
            .await
        {
            tracing::error!(request_id = %prep.request_id, error = %err, "realtime 退款失败（悬置待 sweep）");
        }
        record_failure(state, prep, usage, Some(codes::EMPTY_COMPLETION)).await;
        return;
    }
    let book = state.pricebook.load();
    let quote = match calculate(&book, &prep.calc, usage) {
        Ok(q) => q,
        Err(err) => {
            tracing::error!(request_id = %prep.request_id, error = %err, "realtime 结算算价失败，退款");
            if let Err(err) = state
                .ledger
                .refund(prep.key.user_id, prep.key.key_id, prep.request_id)
                .await
            {
                tracing::error!(request_id = %prep.request_id, error = %err, "realtime 退款失败（悬置待 sweep）");
            }
            record_failure(state, prep, usage, Some("pricing_settle_failed")).await;
            return;
        }
    };
    match state
        .ledger
        .commit(
            prep.key.user_id,
            prep.key.key_id,
            prep.request_id,
            quote.amount,
        )
        .await
    {
        Ok(CommitOutcome::Committed { balance_after, .. }) => {
            let input = SettlementInput {
                request_id: prep.request_id,
                log_type: 2,
                user_id: prep.key.user_id,
                api_key_id: prep.key.key_id,
                group_code: &prep.key.group_code,
                model_name: &prep.canonical,
                channel_id: Some(prep.channel.0),
                channel_key_id: Some(prep.channel.1),
                state: BillingState::Committed,
                usage,
                amount: quote.amount,
                original: quote.original,
                discount: quote.discount,
                pricing_epoch: Some(book.epoch()),
                pricing_snapshot: serde_json::to_value(&quote.snapshot).ok(),
                latency_ms: i32::try_from(prep.started.elapsed().as_millis()).unwrap_or(i32::MAX),
                ttft_ms: None,
                is_stream: true,
                retry_count: 0,
                failover_count: 0,
                upstream_status: Some(101),
                error_code: None,
                upstream_request_id: None,
                node: state.node.as_ref(),
                sticky_layer: 0,
                client_type: "realtime",
                client_ip: None,
                delta_micro: quote.amount.as_micros().saturating_neg(),
                balance_after: Some(balance_after),
                event_type: "commit",
            };
            state.settle_write(input).await;
            super::auth::record_settlement_counters(
                state,
                prep.key.user_id,
                prep.key.member_user_id,
                quote.amount.as_micros(),
                usage.total_raw(),
            )
            .await;
        }
        Ok(CommitOutcome::NoReservation) => {
            tracing::warn!(request_id = %prep.request_id, "realtime 预扣缺失（重复结算/sweep 竞争），跳过");
        }
        Err(err) => {
            tracing::error!(request_id = %prep.request_id, error = %err, "realtime Redis 结算失败（悬置待 sweep）");
        }
    }
}

/// 失败/零产出留痕（log_type 5，delta 0；退款金额语义由 refund Lua 保证）。
async fn record_failure(
    state: &AppState,
    prep: &Prep,
    usage: TokenUsage,
    error_code: Option<&str>,
) {
    let book = state.pricebook.load();
    let input = SettlementInput {
        request_id: prep.request_id,
        log_type: 5,
        user_id: prep.key.user_id,
        api_key_id: prep.key.key_id,
        group_code: &prep.key.group_code,
        model_name: &prep.canonical,
        channel_id: Some(prep.channel.0),
        channel_key_id: Some(prep.channel.1),
        state: BillingState::Failed,
        usage,
        amount: Money::ZERO,
        original: Money::ZERO,
        discount: Money::ZERO,
        pricing_epoch: Some(book.epoch()),
        pricing_snapshot: None,
        latency_ms: i32::try_from(prep.started.elapsed().as_millis()).unwrap_or(i32::MAX),
        ttft_ms: None,
        is_stream: true,
        retry_count: 0,
        failover_count: 0,
        upstream_status: None,
        error_code,
        upstream_request_id: None,
        node: state.node.as_ref(),
        sticky_layer: 0,
        client_type: "realtime",
        client_ip: None,
        delta_micro: 0,
        balance_after: None,
        event_type: "refund",
    };
    state.settle_write(input).await;
}
