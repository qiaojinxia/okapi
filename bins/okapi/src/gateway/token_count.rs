//! Responses 计数预检：鉴权/选路/限流与生成分开，不调用定价、reserve 或 settle。
use super::error::{AppError, with_request_id};
use super::sched_redis::response_affinity::{self, ResponseParent};
use super::sched_redis::token_count::{ChannelPermit, CountPermit};
use super::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_providers::UpstreamError;
use okapi_store::ChannelCandidate;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq)]
enum CountMode {
    Native,
    Auto,
    Estimate,
}

impl CountMode {
    fn from_headers(headers: &HeaderMap) -> Result<Self, AppError> {
        match headers.get("x-okapi-token-count-mode").map(|v| v.to_str()) {
            None | Some(Ok("native")) => Ok(Self::Native),
            Some(Ok("auto")) => Ok(Self::Auto),
            Some(Ok("estimate")) => Ok(Self::Estimate),
            _ => Err(AppError::bad_request().with_param("x-okapi-token-count-mode")),
        }
    }
}

pub async fn responses_input_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = Uuid::new_v4();
    let outcome =
        tokio::time::timeout(Duration::from_secs(60), count(&state, &headers, body)).await;
    match outcome {
        Ok(Ok(response)) => with_request_id(response, request),
        Ok(Err(error)) => error.into_response_with(Some(request)),
        Err(_) => AppError::new(StatusCode::GATEWAY_TIMEOUT, codes::UPSTREAM_TIMEOUT)
            .into_response_with(Some(request)),
    }
}

async fn count(state: &AppState, headers: &HeaderMap, body: Bytes) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    let mode = CountMode::from_headers(headers)?;
    let (requested, value) = validate_request(&body)?;
    let meta = super::chat::resolve_model_cached(state, &requested).await?;
    let meta = meta
        .as_ref()
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND))?;
    if !key.allows_model(&meta.canonical) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::MODEL_NOT_ALLOWED,
        ));
    }
    super::auth::check_group_rate(state, &key).await?;
    let limits = state.setting_cached("model_rpm_limits").await;
    if let Some(limit) = limits
        .as_ref()
        .as_ref()
        .and_then(|v| v.get(&meta.canonical))
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        && !state
            .sched
            .model_rate_ok(key.user_id, &meta.canonical, limit)
            .await
    {
        return Err(
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("model_rpm"),
        );
    }
    // 本地估算不能解析服务端历史，不先探测其是否存在。
    if mode == CountMode::Estimate
        && value
            .get("previous_response_id")
            .is_some_and(|v| !v.is_null())
    {
        return Err(AppError::bad_request().with_param("token_count_estimate_unavailable"));
    }
    let parent =
        response_affinity::resolve_parent(&state.sched, key.user_id, key.key_id, &body).await?;
    let permit = CountPermit::acquire(&state.sched, &key).await?;
    let result = count_authorized(
        state,
        headers,
        mode,
        &requested,
        &meta.canonical,
        value,
        &key.pool_chain(),
        parent.as_ref(),
    )
    .await;
    permit.release().await;
    result
}

/// 本网关按 model 授权和选路，要求明确模型；其余原生字段保留交上游处理。
fn validate_request(body: &Bytes) -> Result<(String, Value), AppError> {
    let mut value: Value = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let fields = value.as_object_mut().ok_or_else(AppError::bad_request)?;
    let model = fields
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::bad_request().with_param("model"))?
        .to_owned();
    for field in ["instructions", "previous_response_id"] {
        if fields
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err(AppError::bad_request().with_param(field));
        }
    }
    if fields.get("input").is_some_and(|v| {
        !v.is_null()
            && !v.is_string()
            && !v.as_array().is_some_and(|a| a.iter().all(Value::is_object))
    }) {
        return Err(AppError::bad_request().with_param("input"));
    }
    if fields
        .get("tools")
        .is_some_and(|v| !v.is_null() && !v.is_array())
    {
        return Err(AppError::bad_request().with_param("tools"));
    }
    if fields.get("conversation").is_some_and(|v| {
        !v.is_null() && !v.is_string() && !v.get("id").is_some_and(Value::is_string)
    }) {
        return Err(AppError::bad_request().with_param("conversation"));
    }
    if fields.get("conversation").is_some_and(|v| !v.is_null())
        && fields
            .get("previous_response_id")
            .is_some_and(|v| !v.is_null())
    {
        return Err(AppError::bad_request().with_param("previous_response_id"));
    }
    if fields
        .get("stream")
        .is_some_and(|v| v != false && !v.is_null())
    {
        return Err(AppError::bad_request().with_param("stream"));
    }
    fields.remove("stream");
    Ok((model, value))
}

#[allow(clippy::too_many_arguments)]
async fn count_authorized(
    state: &AppState,
    headers: &HeaderMap,
    mode: CountMode,
    requested: &str,
    canonical: &str,
    value: Value,
    pools: &[&str],
    parent: Option<&ResponseParent>,
) -> Result<Response, AppError> {
    let body = Bytes::from(serde_json::to_vec(&value).map_err(|_| AppError::bad_request())?);
    let prefs = super::routing_prefs::parse(&body);
    let mut candidates = super::scheduler::order_candidates(
        okapi_store::channels::candidates_for_model(
            &state.pg,
            canonical,
            pools,
            state.master_key.as_deref(),
        )
        .await?,
    );
    candidates.retain(|c| {
        super::routing_prefs::retention_ok(c.data_retention.as_deref(), prefs.zero_retention)
    });
    if candidates.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            if prefs.zero_retention {
                codes::NO_ZERO_RETENTION_CHANNEL
            } else {
                codes::NO_AVAILABLE_CHANNEL
            },
        ));
    }
    if let Some(parent) = parent {
        candidates.retain(|c| parent.binding.matches(c));
        if candidates.is_empty() {
            return Err(response_affinity::unavailable());
        }
    }
    if mode == CountMode::Estimate {
        return estimate_response(candidates[0].upstream_model(canonical), &value);
    }
    let body = super::routing_prefs::strip(&body).unwrap_or(body);
    let mut unsupported = true;
    let mut last_error = AppError::new(StatusCode::NOT_IMPLEMENTED, codes::UPSTREAM_ERROR)
        .with_param("input_tokens_unsupported");
    let mut attempts = 0;
    for candidate in candidates.iter().filter(|c| {
        c.responses_native && c.capabilities.get("input_tokens") != Some(&Value::Bool(false))
    }) {
        if attempts == 3 {
            break;
        }
        if let Some(limit) = candidate.rpm_limit.filter(|n| *n > 0)
            && !state
                .sched
                .channel_key_rate_ok(candidate.channel_key_id, i64::from(limit))
                .await
        {
            unsupported = false;
            last_error = AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("channel_rpm");
            continue;
        }
        let Some(slot) = ChannelPermit::acquire(&state.sched, candidate).await else {
            unsupported = false;
            last_error = AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("channel_concurrency");
            continue;
        };
        attempts += 1;
        let outcome = forward_count(
            state,
            headers,
            candidate,
            requested,
            canonical,
            body.clone(),
        )
        .await;
        slot.release().await;
        match outcome {
            Ok(count) => return Ok(count_response(count.tokens, count.estimated, "upstream")),
            Err(error) => {
                let is_unsupported = matches!(error.upstream_status(), Some(404 | 405 | 501));
                unsupported &= is_unsupported;
                last_error = count_error(&error, is_unsupported);
                if !prefs.allow_fallbacks
                    || parent.is_some()
                    || (!is_unsupported && !error.retriable_before_first_token())
                {
                    return Err(last_error);
                }
            }
        }
    }
    if mode == CountMode::Auto && unsupported && parent.is_none() {
        return estimate_response(candidates[0].upstream_model(canonical), &value);
    }
    Err(last_error)
}

async fn forward_count(
    state: &AppState,
    headers: &HeaderMap,
    candidate: &ChannelCandidate,
    requested: &str,
    canonical: &str,
    body: Bytes,
) -> Result<okapi_providers::responses::InputTokenCount, UpstreamError> {
    let model = candidate.upstream_model(canonical);
    let body = okapi_providers::rewrite_model(&body, requested, model)?;
    let outbound = super::oauth_cred::outbound_with_client(
        candidate,
        &super::oauth_cred::client_headers(headers),
    );
    let oauth = if candidate.provider == "codex" {
        Some(super::oauth_cred::fresh_credential(state, candidate).await?)
    } else {
        None
    };
    let credential = oauth
        .as_ref()
        .map_or(candidate.credential.as_str(), |c| c.access_token.as_str());
    let bearer = format!("Bearer {credential}");
    let mut auth_headers = vec![("authorization", bearer.as_str())];
    if let Some(cred) = oauth.as_ref() {
        if !outbound
            .extra_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("originator"))
        {
            auth_headers.push(("originator", okapi_providers::oauth::codex::ORIGINATOR));
        }
        if let Some(account) = cred.account_id.as_deref() {
            auth_headers.push(("chatgpt-account-id", account));
        }
    }
    let default_base = if oauth.is_some() {
        okapi_providers::oauth::codex::DEFAULT_API_BASE
    } else {
        "https://api.openai.com/v1"
    };
    let base = candidate.api_base.as_deref().unwrap_or(default_base);
    tokio::time::timeout(
        Duration::from_secs(candidate.first_output_timeout_secs.min(20)),
        okapi_providers::responses::count_input_tokens_at(
            state.upstream.http(),
            base,
            &auth_headers,
            body,
            &outbound,
        ),
    )
    .await
    .map_err(|_| UpstreamError::Timeout)?
}

fn count_error(error: &UpstreamError, unsupported: bool) -> AppError {
    if unsupported {
        return AppError::new(StatusCode::NOT_IMPLEMENTED, codes::UPSTREAM_ERROR)
            .with_param("input_tokens_unsupported");
    }
    let status = match error {
        UpstreamError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        UpstreamError::Status {
            status: 400 | 413 | 422,
            ..
        } => StatusCode::BAD_REQUEST,
        UpstreamError::Status { status: 429, .. } => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::BAD_GATEWAY,
    };
    let code = if matches!(error, UpstreamError::Timeout) {
        codes::UPSTREAM_TIMEOUT
    } else {
        codes::UPSTREAM_ERROR
    };
    let mut reply = AppError::new(status, code);
    if let Some(status) = error.upstream_status() {
        reply = reply.with_param(format!("status_{status}"));
    }
    reply
}

fn count_response(tokens: u32, estimated: bool, source: &'static str) -> Response {
    let mut body = json!({"object":"response.input_tokens", "input_tokens":tokens});
    if estimated {
        body["estimated"] = json!(true);
    }
    (
        [
            ("x-okapi-token-count-source", source),
            ("cache-control", "no-store"),
        ],
        axum::Json(body),
    )
        .into_response()
}

/// 显式估算只接受完整的文本上下文。任何服务端引用或多模态/不透明项都交原生计数。
fn estimate_response(model: &str, value: &Value) -> Result<Response, AppError> {
    let unavailable = || AppError::bad_request().with_param("token_count_estimate_unavailable");
    for field in ["previous_response_id", "conversation", "prompt"] {
        if value.get(field).is_some_and(|v| !v.is_null()) {
            return Err(unavailable());
        }
    }
    let mut texts = Vec::new();
    if let Some(text) = value["instructions"].as_str() {
        texts.push(text.to_owned());
    }
    let mut messages = 0;
    match &value["input"] {
        Value::String(text) => {
            texts.push(text.clone());
            messages = 1;
        }
        Value::Array(items) => {
            for item in items {
                match item["type"].as_str() {
                    None | Some("message") => {
                        if !item["role"].is_string() {
                            return Err(unavailable());
                        }
                        messages += 1;
                        match &item["content"] {
                            Value::String(text) => texts.push(text.clone()),
                            Value::Array(parts) => {
                                for part in parts {
                                    if !matches!(
                                        part["type"].as_str(),
                                        Some("input_text" | "output_text")
                                    ) {
                                        return Err(unavailable());
                                    }
                                    texts.push(
                                        part["text"].as_str().ok_or_else(unavailable)?.to_owned(),
                                    );
                                }
                            }
                            _ => return Err(unavailable()),
                        }
                    }
                    Some("function_call") => {
                        texts.push(item["name"].as_str().ok_or_else(unavailable)?.to_owned());
                        texts.push(
                            item["arguments"]
                                .as_str()
                                .ok_or_else(unavailable)?
                                .to_owned(),
                        );
                    }
                    Some("function_call_output") => {
                        texts.push(item["output"].as_str().ok_or_else(unavailable)?.to_owned());
                    }
                    _ => return Err(unavailable()),
                }
            }
        }
        Value::Null => (),
        _ => return Err(unavailable()),
    }
    for field in ["tools", "text", "tool_choice", "reasoning"] {
        if let Some(value) = value.get(field).filter(|v| !v.is_null()) {
            texts.push(value.to_string());
        }
    }
    let segments: Vec<&str> = texts.iter().map(String::as_str).collect();
    Ok(count_response(
        super::estimate::estimate_prompt_tokens(model, &segments, messages),
        true,
        "local_estimate",
    ))
}
