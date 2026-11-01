//! 流式首字前：缓冲首段事件、识别空流 / 空补全 / 请求导致的停止。
use super::super::error::AppError;
use super::super::ingress::Ingress;
use super::super::sched_redis::response_affinity::ResponseWriter;
use super::{
    AttemptError, ForwardFailure, RequestBilling, STARTED_IDLE_WINDOW, cand_info, classify_fatal,
    dispatch_chat, elapsed_ms_i32, first_output_window, response_writer, spawn_stream_pump,
};
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use okapi_api::codes;
use okapi_providers::response_relay::{self, Relayed, capture_relayed};
use okapi_providers::{ChatEvent, ChatResponse, UpstreamError};
use okapi_store::ChannelCandidate;
use okapi_store::channels::KeyFailure;

pub(super) async fn capture_response_event(
    writer: &mut Option<ResponseWriter>,
    bill: &RequestBilling,
    event: &ChatEvent,
) -> Result<(), AppError> {
    if let (Some(writer), ChatEvent::Data { raw, .. }) = (writer, event) {
        writer
            .capture(&bill.state.sched, bill.user_id, bill.key_id, raw.as_bytes())
            .await?;
    }
    Ok(())
}

// ---- 流式 ----

pub(super) fn first_output_buffer_full(buffered: &[ChatEvent]) -> bool {
    buffered.len() >= 256
        || buffered
            .iter()
            .filter_map(|e| match e {
                ChatEvent::Data { raw, .. } => Some(raw.len()),
                ChatEvent::Done => None,
            })
            .sum::<usize>()
            > 16 * 1024 * 1024
}

pub(super) async fn attempt_stream(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    failover: i16,
    sticky_layer: i16,
    retry: i16,
) -> Result<Response, AttemptError> {
    let channel = (cand.channel_id, cand.channel_key_id);
    let connect = capture_relayed(dispatch_chat(bill, cand, base, body, true));
    let first_output_window = first_output_window(cand);
    let (resp, relayed) = match tokio::time::timeout(first_output_window, connect).await {
        Err(_) => {
            bill.trace.failure(&UpstreamError::Timeout);
            return Err(AttemptError::Retriable {
                code: codes::UPSTREAM_TIMEOUT,
                upstream_status: None,
                failure_kind: KeyFailure::Request,
            });
        }
        Ok((Err(err), _)) => return Err(classify_fatal(err, failover, channel)),
        Ok((Ok(resp), relayed)) => (resp, relayed),
    };
    let ChatResponse::Stream(mut handle) = resp else {
        return Err(AttemptError::Retriable {
            code: codes::UPSTREAM_ERROR,
            upstream_status: None,
            failure_kind: KeyFailure::Transient,
        });
    };

    // 首字前只缓冲：窗口内失败/空回复对客户端无痕（§3.7-1/2）
    // 窗口分两段：一个事件都没有按渠道的首字窗口判挂死；上游已经开始回事件（message_start、
    // ping）说明在处理，长上下文的预填充与思考常超过 30 秒，改按空闲计、每来一个事件重新计时。
    let mut buffered: Vec<ChatEvent> = Vec::new();
    let mut note: Option<String> = None;
    let first: Result<Result<bool, UpstreamError>, tokio::time::error::Elapsed> = async {
        let mut window = first_output_window;
        loop {
            match tokio::time::timeout(window, handle.events.next()).await? {
                Some(Ok(event @ ChatEvent::Data { .. })) => {
                    window = first_output_window.max(STARTED_IDLE_WINDOW);
                    let has_output = matches!(
                        event,
                        ChatEvent::Data {
                            has_output: true,
                            ..
                        }
                    );
                    if let Some(error) = stream_failure_in(&event, &mut note) {
                        return Ok(Err(error));
                    }
                    if first_output_buffer_full(&buffered) {
                        return Ok(Err(UpstreamError::Stream(
                            "first_output_buffer_limit".into(),
                        )));
                    }
                    buffered.push(event);
                    if has_output {
                        return Ok(Ok(true));
                    }
                }
                Some(Ok(ChatEvent::Done)) | None => return Ok(Ok(false)),
                Some(Err(err)) => return Ok(Err(err)),
            }
        }
    }
    .await;

    match first {
        Err(_) => {
            // 超时前收到了几个事件：0 = 上游挂死；>0 = 已开始处理但空闲超过窗口
            bill.trace
                .set("first_output_events", serde_json::json!(buffered.len()));
            bill.trace.failure(&UpstreamError::Timeout);
            Err(AttemptError::Retriable {
                code: codes::UPSTREAM_TIMEOUT,
                upstream_status: None,
                failure_kind: KeyFailure::Request,
            })
        }
        Ok(Err(err)) => Err(classify_fatal(err, failover, channel)),
        Ok(Ok(false)) => Err(empty_stream(bill, &buffered, note)),
        Ok(Ok(true)) => {
            let mut writer = response_writer(bill, cand);
            for event in &buffered {
                capture_response_event(&mut writer, bill, event)
                    .await
                    .map_err(|err| {
                        AttemptError::Fatal(ForwardFailure::app(err, failover, Some(channel)))
                    })?;
            }
            let resp = spawn_stream_pump(
                bill.clone(),
                cand_info(
                    cand,
                    &bill.model,
                    bill.is_stream,
                    bill.ingress,
                    sticky_layer,
                    retry,
                ),
                handle,
                buffered,
                writer,
                elapsed_ms_i32(bill.started),
                failover,
            )
            .await;
            Ok(with_relayed(resp, cand, bill, relayed))
        }
    }
}

/// 上游给的、Codex CLI 要读的头回给客户端：轮内粘性令牌带账号标签（见
/// `extensions::turn_state_for_client`）；`openai-model` 等于渠道映射后的上游名时换回客户端
/// 请求的名字（别名与渠道映射都不是改路由，原样回传会让 CLI 误报模型被换；模型级降级才是，
/// 那时回降级到的模型）；`x-reasoning-included` 原样。
pub(super) fn with_relayed(
    mut resp: Response,
    cand: &ChannelCandidate,
    bill: &RequestBilling,
    relayed: Relayed,
) -> Response {
    let model = relayed.model.map(|served| {
        if served != cand.upstream_model(&bill.model) {
            served
        } else if bill.downgraded_from.is_some() {
            bill.model.clone()
        } else {
            bill.requested_model.clone()
        }
    });
    let headers = [
        (
            response_relay::TURN_STATE_HEADER,
            relayed
                .turn_state
                .map(|upstream| super::super::extensions::turn_state_for_client(cand, &upstream)),
        ),
        (response_relay::MODEL_HEADER, model),
        (
            response_relay::REASONING_INCLUDED_HEADER,
            relayed.reasoning_included,
        ),
    ];
    for (name, value) in headers {
        if let Some(value) = value.and_then(|v| axum::http::HeaderValue::from_str(&v).ok()) {
            resp.headers_mut().insert(name, value);
        }
    }
    resp
}

/// 首字缓冲里读到上游在流里说的失败（Responses 的 `error` / `response.failed`）：认得出的类型变成带
/// 等价 HTTP 状态的上游错误，走重试矩阵与 key 冷却，不再混进"空回复"；认不出的把说明记进 `note`，
/// 照旧当空流处理、原因只写进诊断。已经有产出的事件不看。
pub(super) fn stream_failure_in(
    event: &ChatEvent,
    note: &mut Option<String>,
) -> Option<UpstreamError> {
    let ChatEvent::Data {
        raw,
        has_output: false,
        ..
    } = event
    else {
        return None;
    };
    let failure = okapi_providers::stream_failure::detect(raw)?;
    match failure.into_upstream_error() {
        Ok(error) => Some(error),
        Err(failure) => {
            *note = Some(failure.message());
            None
        }
    }
}

/// 空流的收尾：诊断里写上游在流里留下的说明（有的话），其余照旧按空回复处理。
pub(super) fn empty_stream(
    bill: &RequestBilling,
    buffered: &[ChatEvent],
    note: Option<String>,
) -> AttemptError {
    bill.trace.failure(&UpstreamError::Stream(
        note.unwrap_or_else(|| codes::EMPTY_COMPLETION.into()),
    ));
    empty_completion(bill.ingress, buffered)
}

/// 空流照旧换渠道、不计费；只有说不出原因的空流才记成这把 key 的瞬态失败。
pub(super) fn empty_completion(ingress: Ingress, buffered: &[ChatEvent]) -> AttemptError {
    AttemptError::Retriable {
        code: codes::EMPTY_COMPLETION,
        upstream_status: None,
        failure_kind: if request_caused_stop(ingress, buffered) {
            KeyFailure::Request
        } else {
            KeyFailure::Transient
        },
    }
}

/// 空流是不是请求自己收的尾：输出预算耗尽（推理模型把 max_tokens 全花在思考上）或
/// 内容策略拦截。这说明不了凭证好坏——若记成 key 的瞬态失败，调用方用几次极小的
/// max_completion_tokens 就能把整组 key 打进冷却。事件已是客户端方言（转换器已对齐各家原因）。
pub(super) fn request_caused_stop(ingress: Ingress, events: &[ChatEvent]) -> bool {
    use serde_json::Value;
    events.iter().any(|event| {
        let ChatEvent::Data { raw, .. } = event else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return false;
        };
        let reason = |pointer: &str| value.pointer(pointer).and_then(Value::as_str);
        let any_reason = |list: &str, field: &str, stops: &[&str]| {
            value
                .get(list)
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get(field)
                            .and_then(Value::as_str)
                            .is_some_and(|stop| stops.contains(&stop))
                    })
                })
        };
        match ingress {
            Ingress::OpenAi => {
                any_reason("choices", "finish_reason", &["length", "content_filter"])
            }
            Ingress::Anthropic => matches!(
                reason("/delta/stop_reason"),
                Some("max_tokens" | "refusal" | "model_context_window_exceeded")
            ),
            Ingress::Responses | Ingress::ResponsesCompact => matches!(
                reason("/response/incomplete_details/reason"),
                Some("max_output_tokens" | "content_filter")
            ),
            Ingress::Gemini => {
                reason("/promptFeedback/blockReason").is_some_and(|block| !block.is_empty())
                    || any_reason(
                        "candidates",
                        "finishReason",
                        &[
                            "MAX_TOKENS",
                            "SAFETY",
                            "RECITATION",
                            "BLOCKLIST",
                            "PROHIBITED_CONTENT",
                            "SPII",
                            "IMAGE_SAFETY",
                        ],
                    )
            }
        }
    })
}

#[cfg(test)]
mod empty_stop_tests {
    use super::*;

    fn data(raw: &serde_json::Value) -> ChatEvent {
        ChatEvent::Data {
            event: None,
            raw: raw.to_string(),
            content_chars: 0,
            usage: None,
            has_output: false,
        }
    }

    #[test]
    fn budget_and_policy_stops_are_not_credential_failures() {
        for (ingress, raw) in [
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}),
            ),
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}),
            ),
            (
                Ingress::Anthropic,
                serde_json::json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
            ),
            (
                Ingress::Responses,
                serde_json::json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
            ),
            (
                Ingress::Gemini,
                serde_json::json!({"candidates":[{"finishReason":"MAX_TOKENS","content":{"parts":[]}}]}),
            ),
            (
                Ingress::Gemini,
                serde_json::json!({"promptFeedback":{"blockReason":"SAFETY"}}),
            ),
        ] {
            assert!(request_caused_stop(ingress, &[data(&raw)]), "{raw}");
        }
    }

    #[test]
    fn unexplained_empty_streams_still_count_against_the_key() {
        for (ingress, raw) in [
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
            ),
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            ),
            (
                Ingress::Anthropic,
                serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            ),
            (
                Ingress::Gemini,
                serde_json::json!({"candidates":[{"finishReason":"STOP"}]}),
            ),
            // 方言不对的原因字段不能被误认
            (
                Ingress::Anthropic,
                serde_json::json!({"choices":[{"finish_reason":"length"}]}),
            ),
        ] {
            assert!(!request_caused_stop(ingress, &[data(&raw)]), "{raw}");
        }
        assert!(!request_caused_stop(Ingress::OpenAi, &[ChatEvent::Done]));
    }
}
