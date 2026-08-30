//! custom_pass 透传渠道（IMPLEMENTATION §4.4，#1454）：
//! `/pass/{channel_id}/{*path}` 任意方法透明代理到渠道 api_base。
//! 安全两道闸：api_base 固定 + settings.allowed_paths 前缀白名单（空 = 拒绝）。
//! 计费红线：settings.billing_model 必填（models 表 per_call 模型），按次预扣/结算。

use super::error::AppError;
use super::state::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{BillingState, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_ledger::{CommitOutcome, LimitCaps, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, RatioFp, calculate};
use okapi_providers::custom_pass::{PassRequest, PassResponse};
use serde::Deserialize;
use std::time::Instant;
use uuid::Uuid;

/// 渠道 settings 里的透传配置。
#[derive(Deserialize, Default)]
struct PassSettings {
    #[serde(default)]
    allowed_paths: Vec<String>,
    #[serde(default)]
    billing_model: Option<String>,
    #[serde(default)]
    auth_header: Option<String>,
    #[serde(default)]
    auth_scheme: Option<String>,
}

pub async fn custom_pass(
    State(state): State<AppState>,
    Path((channel_id, path)): Path<(i64, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    match handle(
        &state, channel_id, &path, &method, &uri, &headers, body, request_id, started,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

// 鉴权→白名单→按次预扣→透传→结算 的线性时序
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn handle(
    state: &AppState,
    channel_id: i64,
    path: &str,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
    request_id: Uuid,
    started: Instant,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate(state, headers).await?;

    // 渠道点查（透传非热路径，直接 PG；可见性与 chat 同一矩阵语义）
    let channel =
        okapi_store::channels::custom_pass_channel(&state.pg, channel_id, &key.visibility_groups)
            .await?
            .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NO_AVAILABLE_CHANNEL))?;

    let settings: PassSettings =
        serde_json::from_value(channel.settings.clone()).unwrap_or_default();
    // 白名单：前缀匹配，空配置一律拒绝
    let normalized = format!("/{}", path.trim_start_matches('/'));
    if !settings
        .allowed_paths
        .iter()
        .any(|p| normalized.starts_with(p.as_str()))
    {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("path_not_allowed"),
        );
    }
    // 计费模型必填（禁零费裸透传）
    let Some(billing_model) = settings.billing_model.as_deref() else {
        return Err(AppError::bad_request().with_param("billing_model_missing"));
    };

    let book = state.pricebook.load();
    let now = chrono::Utc::now();
    let minute_of_day =
        u16::try_from((now.timestamp().div_euclid(60)).rem_euclid(1440)).unwrap_or(0);
    let calc = CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(billing_model),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: 0,
        local_minute_of_day: minute_of_day,
        now_unix: now.timestamp(),
        surge_active: false,
        service_tier: None,
    };
    // per_call：金额与 usage 无关，预扣即终额
    let quote = calculate(&book, &calc, TokenUsage::default())?;
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

    // —— 预扣已建立：失败路径必须退款 ——
    let url = format!(
        "{}{}{}",
        channel.api_base.trim_end_matches('/'),
        normalized,
        uri.query().map(|q| format!("?{q}")).unwrap_or_default()
    );
    let pass_req = PassRequest {
        method: method.clone(),
        url,
        auth_header: settings
            .auth_header
            .as_deref()
            .unwrap_or("authorization")
            .to_owned(),
        auth_value: format!(
            "{}{}",
            settings.auth_scheme.as_deref().unwrap_or("Bearer "),
            channel.credential
        ),
        content_type: headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body,
    };

    match state.pass.forward(pass_req).await {
        Ok(PassResponse::Ok {
            status,
            content_type,
            stream,
        }) => {
            settle(
                state,
                &key,
                billing_model,
                &quote,
                request_id,
                started,
                true,
                None,
            )
            .await;
            let mut out = Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from_stream(stream))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            if let Ok(value) = axum::http::HeaderValue::from_str(&request_id.to_string()) {
                out.headers_mut().insert("x-okapi-request-id", value);
            }
            Ok(out)
        }
        Ok(PassResponse::ErrStatus { status, body }) => {
            settle(
                state,
                &key,
                billing_model,
                &quote,
                request_id,
                started,
                false,
                Some(i16::try_from(status).unwrap_or(0)),
            )
            .await;
            let out = Response::builder()
                .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
            Ok(out)
        }
        Err(err) => {
            tracing::warn!(request_id = %request_id, error = %err, "custom_pass 上游失败");
            settle(
                state,
                &key,
                billing_model,
                &quote,
                request_id,
                started,
                false,
                None,
            )
            .await;
            Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                codes::UPSTREAM_ERROR,
            ))
        }
    }
}

/// 成功 commit（per_call 终额），失败 refund；均落 billing_records。
#[allow(clippy::too_many_arguments)]
async fn settle(
    state: &AppState,
    key: &okapi_store::AuthedKey,
    billing_model: &str,
    quote: &okapi_pricing::Quote,
    request_id: Uuid,
    started: Instant,
    success: bool,
    upstream_status: Option<i16>,
) {
    let book = state.pricebook.load();
    let (billing_state, log_type, event_type, delta, amount) = if success {
        match state
            .ledger
            .commit(key.user_id, key.key_id, request_id, quote.amount)
            .await
        {
            Ok(CommitOutcome::Committed { .. }) => (
                BillingState::Committed,
                2_i16,
                "commit",
                quote.amount.as_micros().saturating_neg(),
                quote.amount,
            ),
            Ok(CommitOutcome::NoReservation) => return,
            Err(err) => {
                tracing::error!(request_id = %request_id, error = %err, "custom_pass 结算失败（悬置待清理）");
                return;
            }
        }
    } else {
        if let Err(err) = state
            .ledger
            .refund(key.user_id, key.key_id, request_id)
            .await
        {
            tracing::error!(request_id = %request_id, error = %err, "custom_pass 退款失败（悬置待清理）");
        }
        (BillingState::Failed, 5_i16, "refund", 0, Money::ZERO)
    };
    let input = SettlementInput {
        request_id,
        log_type,
        user_id: key.user_id,
        api_key_id: key.key_id,
        group_code: &key.group_code,
        model_name: billing_model,
        channel_id: None,
        channel_key_id: None,
        state: billing_state,
        usage: TokenUsage::default(),
        amount,
        original: quote.original,
        discount: quote.discount,
        pricing_epoch: Some(book.epoch()),
        pricing_snapshot: serde_json::to_value(&quote.snapshot).ok(),
        latency_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
        ttft_ms: None,
        is_stream: false,
        retry_count: 0,
        failover_count: 0,
        upstream_status,
        error_code: (!success).then_some(codes::UPSTREAM_ERROR),
        upstream_request_id: None,
        node: state.node.as_ref(),
        sticky_layer: 0,
        client_type: "custom_pass",
        client_ip: None,
        delta_micro: delta,
        balance_after: None,
        event_type,
    };
    state.settle_write(input).await;
    if success {
        super::auth::record_member_spend(
            state,
            key.user_id,
            key.member_user_id,
            amount.as_micros(),
        )
        .await;
    }
}
