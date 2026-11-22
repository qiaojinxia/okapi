//! 非流式：上游 JSON 应答的取数、非 JSON 2xx 的判定与透传。
use super::super::estimate;
use super::super::ingress::Ingress;
use super::{
    AttemptError, ForwardFailure, RequestBilling, cand_info, classify_fatal, dispatch_chat,
    non_json_failure, non_json_kind, response_writer, settle_commit, with_relayed,
};
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::codes;
use okapi_kernel::error::AppError;
use okapi_kernel::error::with_request_id;
use okapi_providers::response_relay::capture_relayed;
use okapi_providers::{ChatEvent, ChatResponse};
use okapi_store::ChannelCandidate;
use okapi_store::channels::KeyFailure;
use uuid::Uuid;

/// 上游响应元数据（按需采集：model 供响应模型计费、service_tier 供档位计费）。
#[derive(Default, Clone)]
pub(super) struct RespMeta {
    pub(super) model: Option<String>,
    pub(super) service_tier: Option<String>,
}

/// 流式 chunk 的响应元数据采集（开关/tier 定价启用时才调用；首个非空值生效）。
/// chat chunk 的 model/service_tier 在顶层；Responses 直转事件在 `response` 对象内
/// （response.created 就带 model，service_tier 到 response.completed 才定）。
pub(super) fn capture_chunk_meta(event: &ChatEvent, meta: &mut RespMeta) {
    #[derive(serde::Deserialize)]
    struct MetaOnly<'a> {
        /// Gemini 形状叫 modelVersion
        #[serde(default, alias = "modelVersion", borrow)]
        model: Option<std::borrow::Cow<'a, str>>,
        #[serde(default, borrow)]
        service_tier: Option<std::borrow::Cow<'a, str>>,
        #[serde(default, alias = "message", borrow)]
        response: Option<Box<MetaOnly<'a>>>,
    }
    fn absorb(meta: &mut RespMeta, probe: MetaOnly<'_>) {
        if meta.model.is_none()
            && let Some(model) = probe.model.filter(|m| !m.is_empty())
        {
            meta.model = Some(model.into_owned());
        }
        if meta.service_tier.is_none()
            && let Some(tier) = probe.service_tier.filter(|t| !t.is_empty())
        {
            meta.service_tier = Some(tier.into_owned());
        }
        if let Some(inner) = probe.response {
            absorb(meta, *inner);
        }
    }
    if meta.model.is_some() && meta.service_tier.is_some() {
        return;
    }
    let ChatEvent::Data { raw, .. } = event else {
        return;
    };
    let need_model = meta.model.is_none();
    let need_tier = meta.service_tier.is_none();
    // `model` 同时覆盖顶层/嵌套 `model` 与 Gemini 的 `modelVersion`；
    // 所需字段均未出现（且无 `\u` 转义）时直接跳过 JSON 解析，避免长流逐帧重复反序列化。
    if (!need_model || !raw.contains("model"))
        && (!need_tier || !raw.contains("service_tier"))
        && !raw.contains("\\u")
    {
        return;
    }
    if let Ok(probe) = serde_json::from_str::<MetaOnly>(raw) {
        absorb(meta, probe);
    }
}

/// 非流式 JSON 响应的元数据提取（开关/tier 定价启用时才调用）。
pub(super) fn extract_body_meta(body: &Bytes) -> RespMeta {
    #[derive(serde::Deserialize)]
    struct MetaOnly {
        #[serde(alias = "modelVersion")]
        model: Option<String>,
        service_tier: Option<String>,
    }
    serde_json::from_slice::<MetaOnly>(body).map_or_else(
        |_| RespMeta::default(),
        |p| RespMeta {
            model: p.model.filter(|m| !m.is_empty()),
            service_tier: p.service_tier.filter(|t| !t.is_empty()),
        },
    )
}

// ---- 非流式 ----

pub(super) async fn attempt_json(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    failover: i16,
    sticky_layer: i16,
    retry: i16,
) -> Result<Response, AttemptError> {
    let channel = (cand.channel_id, cand.channel_key_id);
    let (result, relayed) = capture_relayed(dispatch_chat(bill, cand, base, body, false)).await;
    match result {
        Ok(ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        }) => {
            // 上游回了 2xx 却不是 JSON 对象（站点的网页前端 / 网关页）：不是一次成功的补全。
            // 不转发、不计费，当作这个候选的瞬态失败换下一个；状态码记真实的 2xx，
            // 同 key 重试只对 5xx / 无状态的瞬态失败生效，这种确定性的失败不必再试一遍。
            if let Some(kind) = non_json_kind(&body) {
                bill.trace.failure(&non_json_failure(kind, body.len()));
                return Err(AttemptError::Retriable {
                    code: codes::UPSTREAM_ERROR,
                    upstream_status: i16::try_from(status).ok(),
                    failure_kind: KeyFailure::Transient,
                    model_fault: None,
                });
            }
            if let Some(mut writer) = response_writer(bill, cand) {
                writer
                    .capture(&bill.state.sched, bill.user_id, bill.key_id, &body)
                    .await
                    .map_err(|err| {
                        AttemptError::Fatal(ForwardFailure::app(err, failover, Some(channel)))
                    })?;
            }
            let content_chars = non_stream_content_chars(bill.ingress, &body);
            let usage = estimate::resolve_usage(
                usage,
                bill.est_prompt,
                content_chars,
                bill.density,
                cand.trust_upstream_usage,
            )
            .map_err(|error| {
                tracing::warn!(request_id = %bill.request_id, %error, "invalid upstream usage");
                AttemptError::Fatal(ForwardFailure::app(
                    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
                    failover,
                    Some(channel),
                ))
            })?;
            let mut info = cand_info(
                cand,
                &bill.model,
                bill.is_stream,
                bill.ingress,
                sticky_layer,
                retry,
            );
            info.upstream_request_id = upstream_request_id;
            let body_meta = extract_body_meta(&body);
            bill.trace.response_model(body_meta.model.as_deref());
            // 响应元数据（model 渠道 opt-in / service_tier 模型配档位倍率）
            let resp_meta = if info.bill_resp_model || bill.has_tier_pricing {
                let mut m = body_meta;
                if !info.bill_resp_model {
                    m.model = None; // 未开开关不按响应模型计费
                }
                m
            } else {
                RespMeta::default()
            };
            // Preserve the independently tracked settlement if the client disconnects,
            // and wait for it before returning a complete JSON response.
            let bill_bg = bill.clone();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            bill.settlement.hand_off();
            bill.state.settlements.spawn(async move {
                let result = settle_commit(&bill_bg, &info, usage, None, failover, resp_meta).await;
                let _ = done_tx.send(result);
            });
            done_rx
                .await
                .unwrap_or_else(|_| Err(AppError::internal()))
                .map_err(|error| {
                    AttemptError::Fatal(ForwardFailure::app(error, failover, Some(channel)))
                })?;
            let mut resp = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            resp = with_request_id(resp, bill.request_id);
            Ok(with_relayed(resp, cand, bill, relayed))
        }
        Ok(ChatResponse::Stream(_)) => Err(AttemptError::Retriable {
            code: codes::UPSTREAM_ERROR,
            upstream_status: None,
            failure_kind: KeyFailure::Transient,
            model_fault: None,
        }),
        Err(err) => Err(classify_fatal(err, failover, channel)),
    }
}

pub(super) fn non_stream_content_chars(ingress: Ingress, body: &Bytes) -> usize {
    use serde_json::Value;
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return 0;
    };
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map_or(0, |t| t.chars().count())
    };
    // 无 usage 时的补全估算输入：全部候选、正文与可见推理，以及工具调用参数——
    // 只数首个候选的正文，n>1 与纯工具调用的回答都会被低估成个位数 token。
    match ingress {
        // output 中保留的用户消息不是新生成内容，密文也不能按字符数估算。
        Ingress::ResponsesCompact => 0,
        Ingress::OpenAi => json_items(v.get("choices"))
            .filter_map(|choice| choice.get("message"))
            .map(|message| {
                let reasoning = message
                    .get("reasoning_content")
                    .or_else(|| message.get("reasoning"));
                text(message.get("content"))
                    + text(reasoning)
                    + text(message.get("refusal"))
                    + json_items(message.get("tool_calls"))
                        .map(|call| text(call.pointer("/function/arguments")))
                        .sum::<usize>()
            })
            .sum(),
        Ingress::Anthropic => json_items(v.get("content"))
            .map(|block| {
                text(block.get("text").or_else(|| block.get("thinking")))
                    + tool_argument_chars(block.get("input"))
            })
            .sum(),
        Ingress::Responses => json_items(v.get("output"))
            .map(|item| match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => json_items(item.get("summary"))
                    .map(|part| text(part.get("text")))
                    .sum(),
                Some("function_call") => tool_argument_chars(item.get("arguments")),
                Some("custom_tool_call") => tool_argument_chars(item.get("input")),
                _ => json_items(item.get("content"))
                    .map(|part| text(part.get("text")))
                    .sum(),
            })
            .sum(),
        Ingress::Gemini => json_items(v.get("candidates"))
            .flat_map(|candidate| json_items(candidate.pointer("/content/parts")))
            .map(|part| {
                text(part.get("text"))
                    + tool_argument_chars(
                        part.pointer("/functionCall/args")
                            .or_else(|| part.pointer("/function_call/args")),
                    )
            })
            .sum(),
    }
}

pub(super) fn json_items(
    value: Option<&serde_json::Value>,
) -> impl Iterator<Item = &serde_json::Value> {
    value
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
}

/// 工具参数的字符数：字符串原样计，结构化值按其 JSON 文本计。
pub(super) fn tool_argument_chars(value: Option<&serde_json::Value>) -> usize {
    match value {
        Some(serde_json::Value::String(s)) => s.chars().count(),
        Some(serde_json::Value::Null) | None => 0,
        Some(value) => value.to_string().chars().count(),
    }
}

// ---- 响应工具 ----

pub(super) fn upstream_passthrough_response(
    ingress: Ingress,
    status: u16,
    body: Bytes,
    request_id: Uuid,
) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let raw_status = status.as_u16();
    // 跨方言错误转译：上游已是入口本方言错误壳时原样透传；否则先结构化提取 message/category，
    // 再按入口协议规范渲染（避免把整段 JSON 塞进 message，也避免 OpenAI 入口透出 Anthropic/Gemini/Bedrock 壳）。
    let body = match ingress {
        Ingress::Anthropic if !body_is_anthropic_error(&body) => {
            okapi_providers::error_normalize::NormalizedError::parse(raw_status, &body)
                .render_anthropic(raw_status)
        }
        Ingress::Gemini if !body_is_gemini_error(&body) => {
            okapi_providers::error_normalize::NormalizedError::parse(raw_status, &body)
                .render_gemini(raw_status, okapi_kernel::error::gemini_status_name(status))
        }
        Ingress::OpenAi | Ingress::Responses | Ingress::ResponsesCompact
            if !body_is_openai_error(&body) =>
        {
            okapi_providers::error_normalize::NormalizedError::parse(raw_status, &body)
                .render_openai(raw_status)
        }
        Ingress::OpenAi
        | Ingress::Responses
        | Ingress::ResponsesCompact
        | Ingress::Anthropic
        | Ingress::Gemini => body,
    };
    let resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    with_request_id(resp, request_id)
}

/// OpenAI 错误壳：`{"error": {...}}` 且既非 Anthropic（顶层 `type == "error"`）也非 Gemini（`error.code` 为数字且含 `error.status`）。
pub(super) fn body_is_openai_error(body: &Bytes) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    if v.get("type").and_then(|t| t.as_str()) == Some("error") {
        return false;
    }
    let Some(err) = v.get("error").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let is_gemini_shape =
        err.get("code").is_some_and(serde_json::Value::is_number) && err.contains_key("status");
    !is_gemini_shape
}

pub(super) fn body_is_anthropic_error(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .is_ok_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("error"))
}

/// google.rpc.Status 壳：`error.code` 为数字且带 `error.status`。
pub(super) fn body_is_gemini_error(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").cloned())
        .is_some_and(|e| {
            e.get("code").is_some_and(serde_json::Value::is_number) && e.get("status").is_some()
        })
}

#[cfg(test)]
mod native_response_meta_tests {
    use super::*;

    #[test]
    fn message_start_model_is_preserved_for_opted_in_response_pricing() {
        let mut meta = RespMeta::default();
        let event = ChatEvent::Data {
            event: Some("message_start".into()),
            raw: serde_json::json!({"type":"message_start","message":{"model":"native-actual"}})
                .to_string(),
            content_chars: 0,
            usage: None,
            has_output: false,
        };
        capture_chunk_meta(&event, &mut meta);
        assert_eq!(meta.model.as_deref(), Some("native-actual"));
        let later = ChatEvent::Data {
            event: None,
            raw: serde_json::json!({"model":"later"}).to_string(),
            content_chars: 0,
            usage: None,
            has_output: false,
        };
        capture_chunk_meta(&later, &mut meta);
        assert_eq!(meta.model.as_deref(), Some("native-actual"));
    }

    #[test]
    fn capture_chunk_meta_fast_path_picks_gemini_model_and_nested_tier() {
        let mut meta = RespMeta::default();
        let make = |raw: &str| ChatEvent::Data {
            event: None,
            raw: raw.to_owned(),
            content_chars: 0,
            usage: None,
            has_output: false,
        };
        // 无关增量直接跳过
        capture_chunk_meta(&make(r#"{"delta":"hello"}"#), &mut meta);
        assert!(meta.model.is_none() && meta.service_tier.is_none());
        // Gemini modelVersion
        capture_chunk_meta(&make(r#"{"modelVersion":"gemini-2.5-pro"}"#), &mut meta);
        assert_eq!(meta.model.as_deref(), Some("gemini-2.5-pro"));
        assert!(meta.service_tier.is_none());
        // Responses 嵌套 service_tier
        capture_chunk_meta(
            &make(r#"{"response":{"model":"ignored","service_tier":"priority"}}"#),
            &mut meta,
        );
        assert_eq!(meta.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(meta.service_tier.as_deref(), Some("priority"));
        // 两者均就位后不再覆盖
        capture_chunk_meta(
            &make(r#"{"model":"other","service_tier":"flex"}"#),
            &mut meta,
        );
        assert_eq!(meta.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(meta.service_tier.as_deref(), Some("priority"));
    }

    #[tokio::test]
    async fn cross_dialect_upstream_errors_normalize_cleanly_while_native_passes_through() {
        use axum::body::to_bytes;
        let rid = Uuid::new_v4();

        // 1. OpenAI 原生错误在 OpenAI 入口原样透传
        let openai_native = Bytes::from_static(
            br#"{"error":{"message":"bad param","type":"invalid_request_error","param":"top_p","code":"invalid_value"}}"#,
        );
        let resp = upstream_passthrough_response(Ingress::OpenAi, 400, openai_native.clone(), rid);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        assert_eq!(body, openai_native);

        // 2. Anthropic 上下文超长错误在 OpenAI 入口转译为规范 OpenAI 错误壳
        let anthropic_err = Bytes::from_static(
            br#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#,
        );
        let resp = upstream_passthrough_response(Ingress::OpenAi, 400, anthropic_err, rid);
        let v: serde_json::Value =
            serde_json::from_slice(&to_bytes(resp.into_body(), 4096).await.unwrap()).unwrap();
        assert!(v.get("type").is_none());
        assert_eq!(v["error"]["code"], "context_length_exceeded");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert_eq!(
            v["error"]["message"],
            "prompt is too long: 210000 tokens > 200000 maximum"
        );

        // 3. OpenAI 错误在 Anthropic 入口不再把整段 JSON 塞进 message
        let openai_ctx = Bytes::from_static(
            br#"{"error":{"message":"This model's maximum context length is 8192 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        );
        let resp = upstream_passthrough_response(Ingress::Anthropic, 400, openai_ctx, rid);
        let v: serde_json::Value =
            serde_json::from_slice(&to_bytes(resp.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert_eq!(
            v["error"]["message"],
            "This model's maximum context length is 8192 tokens."
        );
    }
}
