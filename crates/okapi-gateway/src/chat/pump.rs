//! 流泵：首字后的转发、终止帧闸门、流结束时的结算交接。
use super::super::estimate;
use super::super::execution_plan::{ExecutionPlan, Requirements};
use super::super::ingress::Ingress;
use super::{
    FIRST_OUTPUT_TIMEOUT, ForwardFailure, RequestBilling, RespMeta, capture_chunk_meta,
    capture_response_event, settle_commit, settle_failure,
};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use okapi_api::{UsageProbe, codes};
use okapi_kernel::error::AppError;
use okapi_kernel::error::with_request_id;
use okapi_kernel::sched_redis::response_affinity::ResponseWriter;
use okapi_providers::{ChatEvent, StreamHandle, UpstreamError};
use okapi_store::ChannelCandidate;
use std::convert::Infallible;
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone)]
pub(super) struct CandInfo {
    /// A generated WS turn can have billable usage and still end in an error.
    pub(super) outcome: Option<(i16, String)>,
    pub(super) channel: i64,
    pub(super) cost_milli: i64,
    pub(super) key: i64,
    pub(super) sticky_layer: i16,
    /// 同 key 重试次数（§3.6，记账列 retry_count）。
    pub(super) retry: i16,
    pub(super) upstream_request_id: Option<String>,
    /// 渠道开关：按上游响应模型计费（Sub2API 0.1.175 对齐）。
    pub(super) bill_resp_model: bool,
    /// 渠道开关：不信任上游 usage，结算前本地复核（取两者较大值）。
    pub(super) trust_usage: bool,
    pub(super) upstream_model: String,
    pub(super) upstream_endpoint: String,
}

/// 该渠道的首字窗口：未配 `retry_policy.first_output_timeout_secs` 时用全局缺省。
pub(super) fn first_output_window(cand: &ChannelCandidate) -> Duration {
    if cand.first_output_timeout_secs == FIRST_OUTPUT_TIMEOUT.as_secs() {
        FIRST_OUTPUT_TIMEOUT
    } else {
        Duration::from_secs(cand.first_output_timeout_secs)
    }
}

pub(super) fn cand_info(
    cand: &ChannelCandidate,
    model: &str,
    stream: bool,
    ingress: Ingress,
    sticky_layer: i16,
    retry: i16,
) -> CandInfo {
    CandInfo {
        outcome: None,
        channel: cand.channel_id,
        cost_milli: cand.cost_milli,
        key: cand.channel_key_id,
        sticky_layer,
        retry,
        upstream_request_id: None,
        bill_resp_model: cand.bill_by_response_model,
        trust_usage: cand.trust_upstream_usage,
        upstream_model: cand.upstream_model(model).to_owned(),
        upstream_endpoint: upstream_endpoint(cand, model, stream, ingress).to_owned(),
    }
}
/// 账单与日志里的 `upstream_endpoint`：由这次尝试的执行计划推导（[`ExecutionPlan::endpoint`]），
/// 诊断里记的是同一个标签。编不出计划的候选发不到上游（构造请求体时就被拒），记空串。
pub(super) fn upstream_endpoint(
    cand: &ChannelCandidate,
    model: &str,
    stream: bool,
    ingress: Ingress,
) -> &'static str {
    ExecutionPlan::compile(ingress, cand, model, Requirements::default())
        .map_or("", |plan| plan.endpoint(stream))
}

#[allow(clippy::too_many_lines)]
pub(super) async fn spawn_stream_pump(
    bill: RequestBilling,
    mut info: CandInfo,
    mut handle: StreamHandle,
    buffered: Vec<ChatEvent>,
    mut writer: Option<ResponseWriter>,
    ttft_ms: i32,
    failover: i16,
) -> Response {
    let setting = bill.state.setting_cached("streaming_policy").await;
    let policy = super::super::stream_policy::StreamPolicy::from_setting(setting.as_ref().as_ref());
    info.upstream_request_id = handle.upstream_request_id.take();
    let request_id = bill.request_id;
    let (mut tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);

    // pump 生命周期与上游流绑定；客户端断开经 send 失败感知并取消上游，
    // 结算在任何退出路径都执行（settle_stream）。经 settlements 计数：优雅下线要等它落账。
    let settlements = bill.state.settlements.clone();
    bill.settlement.hand_off();
    settlements.spawn(async move {
        let mut usage: Option<UsageProbe> = None;
        let mut content_chars: usize = 0;
        let mut client_gone = false;
        // Observe the returned model independently of the opt-in billing policy.
        let mut resp_meta = RespMeta::default();
        let mut next_sequence = 0;

        let mut terminal = Vec::new();
        let mut terminal_gate = TerminalGate::new(bill.ingress, bill.choices);
        for event in buffered {
            if let ChatEvent::Data { raw, .. } = &event { bill.trace.stream_event(raw); }
            if resp_meta.model.is_none() || (bill.has_tier_pricing && resp_meta.service_tier.is_none()) {
                capture_chunk_meta(&event, &mut resp_meta);
            }
            if terminal_gate.defer(&event) {
                capture_terminal_usage(&event, &mut usage, &mut content_chars);
                terminal.push(event);
                continue;
            }
            advance_stream_sequence(&event, bill.ingress, &mut next_sequence);
            if !push_event(
                &mut tx,
                &event,
                bill.ingress,
                &mut usage,
                &mut content_chars,
            )
            .await
            {
                client_gone = true;
                break;
            }
        }
        let mut saw_done = false;
        while !client_gone && !saw_done {
            let next = tokio::time::timeout_at(policy.deadline(tokio::time::Instant::from_std(bill.started + Duration::from_mins(8))), handle.events.next()).await
                .unwrap_or(Some(Err(UpstreamError::Timeout)));
            match next {
                Some(Ok(event)) => {
                    if let ChatEvent::Data { raw, .. } = &event { bill.trace.stream_event(raw); }
                    if resp_meta.model.is_none() || (bill.has_tier_pricing && resp_meta.service_tier.is_none()) {
                        capture_chunk_meta(&event, &mut resp_meta);
                    }
                    if let Err(err) = capture_response_event(&mut writer, &bill, &event).await {
                        // 首字已发送：终止而不改投，保留终态 usage 供实际产出结算。
                        if let ChatEvent::Data { usage: Some(reported), .. } = &event { usage = Some(reported.with_previous(usage)); }
                        let event = super::super::ingress::stream_error_event(bill.ingress, &err, request_id, Some(next_sequence));
                        let _ = tokio::time::timeout(Duration::from_secs(5), tx.send(Ok(event))).await;
                        break;
                    }
                    saw_done = matches!(event, ChatEvent::Done);
                    if terminal_gate.defer(&event) {
                        capture_terminal_usage(&event, &mut usage, &mut content_chars);
                        if terminal.len() >= 16 { break; }
                        terminal.push(event);
                        continue;
                    }
                    advance_stream_sequence(&event, bill.ingress, &mut next_sequence);
                    if !push_event(
                        &mut tx,
                        &event,
                        bill.ingress,
                        &mut usage,
                        &mut content_chars,
                    )
                    .await
                    {
                        client_gone = true;
                    }
                }
                // 首字后断流：不可回退，按已产出结算（§3.6）
                Some(Err(err)) => {
                    bill.trace.failure(&err);
                    bill.trace.set("request_failed", serde_json::json!(true));
                    bill.trace.set("stream_end_reason", serde_json::json!("upstream_error"));
                    info.outcome = Some((err.upstream_status().unwrap_or(502), err.error_code().into()));
                    tracing::warn!(request_id = %request_id, error = %err, "首字后断流，按已产出结算");
                    let error = AppError::new(if matches!(err, UpstreamError::Timeout | UpstreamError::Unreachable { timed_out: true, .. }) { StatusCode::GATEWAY_TIMEOUT } else { StatusCode::BAD_GATEWAY }, err.error_code());
                    let event = super::super::ingress::stream_error_event(bill.ingress, &error, request_id, Some(next_sequence));
                    let _ = tokio::time::timeout(Duration::from_secs(5),tx.send(Ok(event))).await;
                    break;
                }
                None => break,
            }
        }
        drop(handle); // 取消上游（客户端断开路径）
        bill.trace.response_model(resp_meta.model.as_deref());
        if info.outcome.is_none() && bill.trace.snapshot()["request_failed"] == true {
            info.outcome = Some((502, codes::UPSTREAM_ERROR.into()));
        }
        if bill.trace.snapshot().get("stream_end_reason").is_none() {
            bill.trace.set("stream_end_reason", serde_json::json!(if client_gone { "client_closed" } else if saw_done { "completed" } else { "upstream_closed" }));
        }
        if !info.bill_resp_model { resp_meta.model = None; }
        let result = settle_stream(
            &bill,
            &info,
            usage,
            content_chars,
            ttft_ms,
            failover,
            client_gone,
            resp_meta,
        )
        .await;
        finish_settled_stream(tx, result, terminal, bill.ingress, request_id, next_sequence).await;
        // 结算完成后释放渠道 key 并发信号量（§3.5）
    });

    let sse = Sse::new(rx).keep_alive(KeepAlive::new().interval(policy.heartbeat).text("ping"));
    with_request_id(
        super::super::stream_policy::unbuffered(sse.into_response()),
        request_id,
    )
}

pub(super) fn capture_terminal_usage(
    event: &ChatEvent,
    usage: &mut Option<UsageProbe>,
    chars: &mut usize,
) {
    if let ChatEvent::Data {
        usage: reported,
        content_chars,
        ..
    } = event
    {
        if let Some(reported) = reported {
            *usage = Some(reported.with_previous(*usage));
        }
        *chars = chars.saturating_add(*content_chars);
    }
}

pub(super) fn advance_stream_sequence(event: &ChatEvent, ingress: Ingress, next: &mut u64) {
    if matches!(ingress, Ingress::Responses | Ingress::ResponsesCompact)
        && let ChatEvent::Data { raw, .. } = event
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(sequence) = value
            .get("sequence_number")
            .and_then(serde_json::Value::as_u64)
    {
        *next = (*next).max(sequence.saturating_add(1));
    }
}

pub(super) async fn finish_settled_stream(
    mut tx: mpsc::Sender<Result<Event, Infallible>>,
    result: Result<(), AppError>,
    terminal: Vec<ChatEvent>,
    ingress: Ingress,
    request_id: Uuid,
    next_sequence: u64,
) {
    if let Err(error) = result {
        tracing::error!(%request_id, ?error, "stream usage persistence failed");
        let event = super::super::ingress::stream_error_event(
            ingress,
            &error,
            request_id,
            Some(next_sequence),
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), tx.send(Ok(event))).await;
    } else {
        let mut usage = None;
        let mut chars = 0;
        for event in terminal {
            if !push_event(&mut tx, &event, ingress, &mut usage, &mut chars).await {
                break;
            }
        }
    }
}

/// 结尾帧扣到结算之后再投递。命名的终止事件与 `[DONE]` 即结尾；Gemini 没有独立的终止帧，
/// 带 finishReason 的块就是结尾——但多候选（candidateCount>1）时各候选先后结束，只有收齐全部
/// 候选结束标记的那一块才算，之后的块也一并扣住保序。只看"任一候选带 finishReason"会把先结束
/// 的候选那一块扣住、让别的候选的后续增量先到（客户端顺序就乱了），结尾满 16 条还会丢帧。
pub(super) struct TerminalGate {
    pub(super) ingress: Ingress,
    pub(super) candidates: usize,
    pub(super) finished: std::collections::HashSet<u64>,
    pub(super) closed: bool,
}

impl TerminalGate {
    fn new(ingress: Ingress, candidates: u32) -> Self {
        Self {
            ingress,
            candidates: usize::try_from(candidates.max(1)).unwrap_or(1),
            finished: std::collections::HashSet::new(),
            closed: false,
        }
    }

    fn defer(&mut self, event: &ChatEvent) -> bool {
        let ChatEvent::Data {
            event: name, raw, ..
        } = event
        else {
            return true;
        };
        if matches!(
            name.as_deref(),
            Some("message_stop" | "response.completed" | "response.failed" | "response.incomplete")
        ) {
            return true;
        }
        if self.ingress != Ingress::Gemini {
            return false;
        }
        if self.closed {
            return true;
        }
        let Ok(chunk) = serde_json::from_str::<serde_json::Value>(raw) else {
            return false;
        };
        let candidates = chunk
            .get("candidates")
            .and_then(serde_json::Value::as_array);
        for candidate in candidates.into_iter().flatten() {
            if candidate
                .get("finishReason")
                .is_some_and(|reason| !reason.is_null())
            {
                let index = candidate.get("index").and_then(serde_json::Value::as_u64);
                self.finished.insert(index.unwrap_or(0));
            }
        }
        self.closed = self.finished.len() >= self.candidates;
        self.closed
    }
}

/// 返回 false 表示客户端已断开。
/// 终止符按入口方言：OpenAI 发 `data: [DONE]`；Anthropic 以 message_stop 事件收尾
/// （已作为 Data 透出），Done 不再发帧。
pub(super) async fn push_event(
    tx: &mut mpsc::Sender<Result<Event, Infallible>>,
    event: &ChatEvent,
    ingress: Ingress,
    usage: &mut Option<UsageProbe>,
    content_chars: &mut usize,
) -> bool {
    let sse_event = match event {
        ChatEvent::Data {
            raw,
            event: name,
            content_chars: chars,
            usage: ev_usage,
            ..
        } => {
            *content_chars = content_chars.saturating_add(*chars);
            if let Some(reported) = ev_usage {
                *usage = Some(reported.with_previous(*usage));
            }
            let mut ev = Event::default();
            if let Some(name) = name {
                ev = ev.event(name);
            }
            ev.data(raw)
        }
        ChatEvent::Done => match ingress {
            Ingress::OpenAi => Event::default().data("[DONE]"),
            // Anthropic 以 message_stop、Responses 以 response.completed、Gemini 以带
            // finishReason 的 chunk 收尾，均无终止帧
            Ingress::Anthropic
            | Ingress::Responses
            | Ingress::ResponsesCompact
            | Ingress::Gemini => return true,
        },
    };
    tokio::time::timeout(Duration::from_secs(10), tx.send(Ok(sse_event)))
        .await
        .is_ok_and(|result| result.is_ok())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn settle_stream(
    bill: &RequestBilling,
    info: &CandInfo,
    usage: Option<UsageProbe>,
    content_chars: usize,
    ttft_ms: i32,
    failover: i16,
    client_gone: bool,
    resp_meta: RespMeta,
) -> Result<(), AppError> {
    let usage = match estimate::resolve_usage(
        usage,
        bill.est_prompt,
        content_chars,
        bill.density,
        info.trust_usage,
    ) {
        Ok(usage) => usage,
        Err(error) => {
            tracing::warn!(request_id = %bill.request_id, %error, "invalid upstream usage");
            let error = AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR);
            let failure = ForwardFailure::app(error, failover, Some((info.channel, info.key)));
            settle_failure(bill, &failure).await;
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                codes::UPSTREAM_ERROR,
            ));
        }
    };
    if client_gone {
        tracing::info!(request_id = %bill.request_id, "客户端提前断开，按已产出结算");
    }
    settle_commit(bill, info, usage, Some(ttft_ms), failover, resp_meta).await
}

#[cfg(test)]
mod upstream_endpoint_tests {
    use super::{Ingress, upstream_endpoint};

    #[test]
    fn subscription_anthropic_is_labelled_as_messages() {
        let mut cand = crate::scheduler::tests::cand(0, 1, 1000);
        for provider in ["anthropic", "anthropic_max"] {
            cand.provider = provider.into();
            assert_eq!(
                upstream_endpoint(&cand, "claude-x", true, Ingress::Anthropic),
                "/v1/messages",
                "{provider}"
            );
        }
        cand.provider = "openai".into();
        assert_eq!(
            upstream_endpoint(&cand, "gpt-x", false, Ingress::OpenAi),
            "/v1/chat/completions"
        );
    }

    /// 编不出执行计划的候选（这里是只做透传的 custom_pass）发不到上游，不编一个端点出来。
    #[test]
    fn an_unplannable_candidate_has_no_upstream_endpoint() {
        let mut cand = crate::scheduler::tests::cand(0, 1, 1000);
        cand.provider = "custom_pass".into();
        assert_eq!(upstream_endpoint(&cand, "m", false, Ingress::OpenAi), "");
    }
}

#[cfg(test)]
mod terminal_gate_tests {
    use super::{ChatEvent, Ingress, TerminalGate};

    fn chunk(raw: &str) -> ChatEvent {
        ChatEvent::Data {
            event: None,
            has_output: true,
            content_chars: 0,
            usage: None,
            raw: raw.to_owned(),
        }
    }

    #[test]
    fn gemini_holds_back_only_the_chunk_that_finishes_every_candidate() {
        let mut gate = TerminalGate::new(Ingress::Gemini, 2);
        let first_done = r#"{"candidates":[{"index":0,"content":{"parts":[{"text":"a"}]},"finishReason":"STOP"},{"index":1,"content":{"parts":[{"text":"b"}]}}]}"#;
        assert!(!gate.defer(&chunk(first_done)), "只结束了候选 0，照常转发");
        assert!(!gate.defer(&chunk(
            r#"{"candidates":[{"index":1,"content":{"parts":[{"text":"c"}]}}]}"#
        )));
        assert!(
            gate.defer(&chunk(
                r#"{"candidates":[{"index":1,"finishReason":"STOP"}]}"#
            )),
            "收齐两个候选才算结尾"
        );
        assert!(
            gate.defer(&chunk(r#"{"usageMetadata":{"totalTokenCount":9}}"#)),
            "结尾之后的块一并扣住保序"
        );
    }

    #[test]
    fn single_candidate_and_other_ingresses_keep_their_terminals() {
        let mut gate = TerminalGate::new(Ingress::Gemini, 1);
        assert!(!gate.defer(&chunk(
            r#"{"candidates":[{"content":{"parts":[{"text":"a"}]},"finishReason":null}]}"#
        )));
        assert!(gate.defer(&chunk(r#"{"candidates":[{"finishReason":"STOP"}]}"#)));
        let mut gate = TerminalGate::new(Ingress::OpenAi, 1);
        assert!(!gate.defer(&chunk(
            r#"{"choices":[{"delta":{"content":"a"},"finish_reason":"stop"}]}"#
        )));
        assert!(gate.defer(&ChatEvent::Done));
    }
}
