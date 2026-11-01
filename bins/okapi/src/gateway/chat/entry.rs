//! HTTP 入口：chat completions / responses（含 compact）/ gemini generateContent / messages。
use super::super::error::AppError;
use super::super::estimate::estimate_prompt_tokens;
use super::super::ingress::Ingress;
use super::super::sched_redis::session_hash;
use super::super::state::AppState;
use super::{ProbeInfo, check_input, handle_chat, request_features};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use okapi_api::{ChatRequestProbe, MessagesRequestProbe, ResponsesRequestProbe, codes};
use std::time::Instant;
use uuid::Uuid;

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let authenticated = match super::super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_response_with(Some(request_id)),
    };
    // 请求体只解析成 Value 一次，探针从 Value 取（不再按字节各解析一遍）
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    let Ok(probe) = <ChatRequestProbe as serde::Deserialize>::deserialize(&value) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    if let Err(error) = check_input(Ingress::OpenAi, &value) {
        return error.into_response_with(Some(request_id));
    }
    let features = request_features(Ingress::OpenAi, &value);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.max_completion_tokens.or(probe.max_tokens),
        choices: probe.choices(),
        prompt_tokens: estimate_prompt_tokens(
            &probe.model,
            &probe.prompt_segments(),
            probe.messages.len(),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&headers, None, &probe.messages),
        service_tier: probe.service_tier.clone(),
        needs_tools: features.tools,
        needs_vision: features.vision,
        foreign_tools: features.foreign_tools,
    };
    match Box::pin(handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::OpenAi,
        &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// OpenAI /v1/responses 入口（§4.4：渠道说 Responses 方言则直转，否则降级 ChatCompletions #5209）。
pub async fn responses(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    Box::pin(responses_entry(state, headers, body, Ingress::Responses)).await
}

pub async fn responses_compact(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    Box::pin(responses_entry(
        state,
        headers,
        body,
        Ingress::ResponsesCompact,
    ))
    .await
}

pub(super) fn compact_request_body(body: &Bytes) -> Result<Bytes, serde_json::Error> {
    let mut fields: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(body)?;
    fields.remove("stream");
    serde_json::to_vec(&fields).map(Bytes::from)
}

pub(super) async fn responses_entry(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
    ingress: Ingress,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let authenticated = match super::super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_response_with(Some(request_id)),
    };
    // 请求体只解析成 Value 一次，探针从 Value 取（不再按字节各解析一遍）
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    let Ok(probe) = <ResponsesRequestProbe as serde::Deserialize>::deserialize(&value) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    if let Err(error) = check_input(Ingress::Responses, &value) {
        return error.into_response_with(Some(request_id));
    }
    // 后台模式先回「排队中」、不带用量，生成在本次结算之后继续——网关只能按估算收一点，
    // 推理费用全由站方承担，且没有取回结果的接口。WS 入口同样拒绝。
    if probe.background == Some(true) {
        return AppError::bad_request()
            .with_param("background")
            .into_response_with(Some(request_id));
    }
    let body = if ingress == Ingress::ResponsesCompact {
        if probe.stream {
            return AppError::bad_request()
                .with_param("stream")
                .into_response_with(Some(request_id));
        }
        // 通用字段剥除器会保护 stream；compact 协议须单独移除该字段。
        let Ok(body) = compact_request_body(&body) else {
            return AppError::bad_request().into_response_with(Some(request_id));
        };
        body
    } else {
        body
    };
    let input_messages = probe.input_messages();
    let features = request_features(ingress, &value);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.completion_cap_req(),
        choices: 1,
        prompt_tokens: estimate_prompt_tokens(
            &probe.model,
            &probe.prompt_segments(),
            input_messages.len().max(1),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&headers, probe.prompt_cache_key.as_deref(), &input_messages),
        service_tier: probe.service_tier.clone(),
        needs_tools: features.tools,
        needs_vision: features.vision,
        foreign_tools: features.foreign_tools,
    };
    match Box::pin(handle_chat(
        &state, &headers, &body, request_id, started, ingress, &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// Gemini 入口查询串：只认 `key`（SDK 鉴权）；`alt=sse` 等其余参数忽略。
#[derive(serde::Deserialize)]
pub struct GeminiQuery {
    #[serde(default)]
    pub key: Option<String>,
}

/// Gemini 原生入口 `POST /v1beta/models/{model}:generateContent|streamGenerateContent`。
/// 模型名与流式与否都在路径上（`gemini-2.5-pro:streamGenerateContent`）；鉴权除 Bearer 外
/// 认 Gemini SDK 的 `x-goog-api-key` 头与 `?key=` 查询串。流式一律以 SSE 回（官方 `alt=sse`
/// 形态；不带 alt 的 JSON 数组流式形态不提供）。
pub async fn gemini_generate(
    State(state): State<AppState>,
    axum::extract::Path(model_action): axum::extract::Path<String>,
    uri: axum::http::Uri,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    // 自己解析查询串：axum 的 Query 拒绝时回框架的纯文本 400（如重复的 `?key=`），不走 Gemini 错误壳
    let query: GeminiQuery = match super::super::extract::query_from_uri(&uri) {
        Ok(query) => query,
        Err(error) => return error.into_gemini_response_with(Some(request_id)),
    };
    let Some((model, action)) = model_action.rsplit_once(':') else {
        return AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND)
            .into_gemini_response_with(Some(request_id));
    };
    let stream = match action {
        "generateContent" => false,
        "streamGenerateContent" => true,
        _ => {
            return AppError::bad_request()
                .with_param(action)
                .into_gemini_response_with(Some(request_id));
        }
    };
    // `?key=` 查询串鉴权：搬进头里让 authenticate 统一处理（不改鉴权主链）
    if !headers.contains_key(header::AUTHORIZATION)
        && !headers.contains_key("x-goog-api-key")
        && let Some(key) = query.key.as_deref()
        && let Ok(value) = axum::http::HeaderValue::from_str(key)
    {
        headers.insert("x-goog-api-key", value);
    }
    let authenticated = match super::super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_gemini_response_with(Some(request_id)),
    };
    // 请求体只解析成 Value 一次，探针从 Value 取（不再按字节各解析一遍）
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return AppError::bad_request().into_gemini_response_with(Some(request_id));
    };
    let Ok(probe) = <okapi_api::GeminiRequestProbe as serde::Deserialize>::deserialize(&value)
    else {
        return AppError::bad_request().into_gemini_response_with(Some(request_id));
    };
    if let Err(error) = check_input(Ingress::Gemini, &value) {
        return error.into_gemini_response_with(Some(request_id));
    }
    let input_messages = probe.input_messages();
    let features = request_features(Ingress::Gemini, &value);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: model.to_owned(),
        stream,
        completion_cap_req: probe.completion_cap_req(),
        choices: probe.choices(),
        prompt_tokens: estimate_prompt_tokens(
            model,
            &probe.prompt_segments(),
            input_messages.len().max(1),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&headers, None, &input_messages),
        service_tier: None,
        needs_tools: features.tools,
        needs_vision: features.vision,
        foreign_tools: features.foreign_tools,
    };
    match Box::pin(handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::Gemini,
        &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_gemini_response_with(Some(request_id)),
    }
}

/// Anthropic /v1/messages 入口（§4.4：入口协议 + 上游方向双向）。
pub async fn messages(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let authenticated = match super::super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_anthropic_response_with(Some(request_id)),
    };
    // 请求体只解析成 Value 一次，探针从 Value 取（不再按字节各解析一遍）
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return AppError::bad_request().into_anthropic_response_with(Some(request_id));
    };
    let Ok(probe) = <MessagesRequestProbe as serde::Deserialize>::deserialize(&value) else {
        return AppError::bad_request().into_anthropic_response_with(Some(request_id));
    };
    if let Err(error) = check_input(Ingress::Anthropic, &value) {
        return error.into_anthropic_response_with(Some(request_id));
    }
    let features = request_features(Ingress::Anthropic, &value);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.max_tokens,
        choices: 1,
        prompt_tokens: estimate_prompt_tokens(
            &probe.model,
            &probe.prompt_segments(),
            probe.messages.len(),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(
            &headers,
            probe.client_session_id().as_deref(),
            &probe.messages,
        ),
        service_tier: None,
        needs_tools: features.tools,
        needs_vision: features.vision,
        foreign_tools: features.foreign_tools,
    };
    match Box::pin(handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::Anthropic,
        &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_anthropic_response_with(Some(request_id)),
    }
}
