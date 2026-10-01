use super::super::{
    AppError, ChatEvent, FailureReply, ForwardFailure, Ingress, ProbeInfo, RequestBilling,
    RespMeta, ResponsesRequestProbe, StatusCode, UsageProbe, capture_chunk_meta,
    capture_response_event, codes, elapsed_ms_i32, estimate_prompt_tokens, prepare_chat,
    request_features, session_hash, settle_failure, settle_stream,
};
use super::{Session, Work, bridge, history, routing};
use futures::StreamExt;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::time::Instant as Deadline;

fn report(session: &Session, work: &Work, failure: &ForwardFailure) {
    match &failure.reply {
        FailureReply::App(error) => session.output.error(error, work.request, work.lane.clone()),
        FailureReply::Upstream { status, body } => {
            // Preserve a valid provider error envelope, adding lane/request correlation.
            if let Ok(mut value) = serde_json::from_slice::<Value>(body)
                && value.is_object()
                && value.get("error").is_some()
            {
                value["type"] = "error".into();
                value["status"] = (*status).into();
                if let Some(lane) = &work.lane {
                    value["stream_id"] = lane.clone().into();
                } else if let Some(object) = value.as_object_mut() {
                    object.remove("stream_id");
                }
                session.output.data(&value.to_string(), work.request);
            } else {
                session.output.error(
                    &AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
                    work.request,
                    work.lane.clone(),
                );
            }
        }
    }
}

fn probe(session: &Session, work: &Work, body: &super::Bytes) -> Result<ProbeInfo, AppError> {
    let probe: ResponsesRequestProbe =
        serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let messages = probe.input_messages();
    let (needs_tools, needs_vision) = request_features(Ingress::Responses, body);
    Ok(ProbeInfo {
        requested_model: probe.model.clone(),
        stream: true,
        completion_cap_req: if work.warmup {
            Some(0)
        } else {
            probe.completion_cap_req()
        },
        prompt_tokens: estimate_prompt_tokens(
            &probe.model,
            &probe.prompt_segments(),
            messages.len().max(1),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&session.headers, &messages),
        needs_tools,
        needs_vision,
        service_tier: probe.service_tier.clone(),
    })
}

pub(super) async fn run(session: &Session, work: &Work) {
    let failed = execute(session, work).await;
    session.history.lock().await.finish(
        work.request,
        &work.lane,
        bridge::previous(work).as_deref(),
        failed,
    );
}

async fn execute(session: &Session, work: &Work) -> bool {
    if session.output.is_closed() {
        return true;
    }
    let started = Instant::now();
    let context = match session.history.lock().await.lookup(&work.body) {
        Ok(context) => context,
        Err(error) => {
            session
                .output
                .error(&error, work.request, work.lane.clone());
            return true;
        }
    };
    let estimate = match history::estimate(&work.body, context.as_deref()) {
        Ok(body) => body,
        Err(error) => {
            session
                .output
                .error(&error, work.request, work.lane.clone());
            return true;
        }
    };
    let info = match probe(session, work, &estimate) {
        Ok(info) => info,
        Err(error) => {
            session
                .output
                .error(&error, work.request, work.lane.clone());
            return true;
        }
    };
    let _inflight = session.state.in_flight.enter().await;
    let bill = match prepare_chat(
        &session.state,
        &session.headers,
        &work.body,
        work.request,
        started,
        Ingress::Responses,
        &info,
    )
    .await
    {
        Ok(bill) => bill,
        Err(error) => {
            session
                .output
                .error(&error, work.request, work.lane.clone());
            return true;
        }
    };
    if (bill.user_id, bill.key_id) != session.principal {
        let failure = ForwardFailure::app(
            AppError::new(StatusCode::UNAUTHORIZED, codes::INVALID_API_KEY),
            0,
            None,
        );
        report(session, work, &failure);
        settle_failure(&bill, &failure).await;
        return true;
    }
    let mut routed = match routing::route(session, &bill, &info, work, context.as_deref()).await {
        Ok(routed) => routed,
        Err(failure) => {
            report(session, work, &failure);
            settle_failure(&bill, &failure).await;
            return true;
        }
    };
    let progress = consume(session, work, &bill, &mut routed).await;
    if let Some(bridge) = &routed.bridge
        && let Ok(id) = bridge.request_id.lock()
    {
        routed.info.upstream_request_id.clone_from(&id);
    }
    let failed = progress.error.is_some() || !progress.terminal;
    progress.settle(session, work, &bill, &mut routed).await;
    bill.state
        .sched
        .release_slot(routed.info.key, routed.info.cap)
        .await;
    failed
}

#[derive(Default)]
struct Progress {
    usage: Option<UsageProbe>,
    chars: usize,
    output_seen: bool,
    meta: RespMeta,
    ttft: Option<i32>,
    terminal: bool,
    error: Option<AppError>,
    suppressed: bool,
}

impl Progress {
    fn fail(&mut self, session: &Session, work: &Work, error: AppError) {
        if self.error.is_none() {
            if !self.suppressed {
                session
                    .output
                    .error(&error, work.request, work.lane.clone());
            }
            self.error = Some(error);
        }
        self.suppressed = true;
    }

    async fn event(
        &mut self,
        session: &Session,
        work: &Work,
        bill: &RequestBilling,
        routed: &mut routing::Routed,
        event: &ChatEvent,
    ) {
        let ChatEvent::Data {
            raw,
            usage,
            content_chars,
            has_output,
            ..
        } = event
        else {
            return;
        };
        if usage.is_some() {
            self.usage = *usage;
        }
        self.chars = self.chars.saturating_add(*content_chars);
        self.output_seen |= has_output;
        if *has_output && self.ttft.is_none() {
            self.ttft = Some(elapsed_ms_i32(bill.started));
        }
        capture_chunk_meta(event, &mut self.meta);
        bill.trace.stream_event(raw);
        let value = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.terminal = matches!(
            kind,
            "response.completed" | "response.failed" | "response.incomplete" | "error"
        );
        if self.error.is_none()
            && matches!(kind, "error" | "response.failed" | "response.incomplete")
        {
            let status = value
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|v| u16::try_from(v).ok())
                .and_then(|v| StatusCode::from_u16(v).ok())
                .filter(|s| s.is_client_error() || s.is_server_error())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            self.error = Some(AppError::new(status, codes::UPSTREAM_ERROR));
        }
        if self.terminal && work.warmup && self.usage.is_none() && self.error.is_none() {
            self.fail(
                session,
                work,
                AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
                    .with_param("usage_missing"),
            );
        }
        if !self.suppressed
            && let Some(bridge) = &mut routed.bridge
            && let Err(error) = bridge.capture.event(
                &mut *session.history.lock().await,
                &work.lane,
                work.request,
                raw,
            )
        {
            self.fail(session, work, error);
        }
        if !self.suppressed {
            if let Err(error) = capture_response_event(&mut routed.writer, bill, event).await {
                self.fail(session, work, error);
            } else {
                session.output.data(raw, work.request);
            }
        }
    }

    async fn settle(
        mut self,
        session: &Session,
        work: &Work,
        bill: &RequestBilling,
        routed: &mut routing::Routed,
    ) {
        if !self.terminal && self.error.is_none() {
            self.fail(
                session,
                work,
                AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
            );
        }
        if let Some(error) = &self.error {
            bill.trace.set("request_failed", serde_json::json!(true));
            bill.trace
                .set("stream_end_reason", serde_json::json!("upstream_error"));
            routed.info.outcome = Some((
                i16::try_from(error.status.as_u16()).unwrap_or(502),
                error.code.clone(),
            ));
        }
        bill.trace.response_model(self.meta.model.as_deref());
        if !routed.info.bill_resp_model {
            self.meta.model = None;
        }
        if let Some(error) = self.error
            && self
                .usage
                .is_none_or(|u| u.prompt_tokens == 0 && u.completion_tokens == 0)
            && !self.output_seen
        {
            let mut failure = ForwardFailure::app(
                error,
                routed.failover,
                Some((routed.info.channel, routed.info.key)),
            );
            failure.upstream_status = routed.info.outcome.as_ref().map(|v| v.0);
            failure.upstream = Some(Box::new((
                routed.info.upstream_model.clone(),
                routed.info.upstream_endpoint.clone(),
            )));
            settle_failure(bill, &failure).await;
        } else if routed
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.local_warmup)
        {
            bridge::settle_warmup(bill, &routed.info).await;
        } else {
            // Warmup usage is authoritative: no fabricated output or local recount.
            if work.warmup {
                routed.info.trust_usage = true;
            }
            if let Err(error) = settle_stream(
                bill,
                &routed.info,
                self.usage,
                self.chars,
                self.ttft.unwrap_or_else(|| elapsed_ms_i32(bill.started)),
                routed.failover,
                session.output.is_closed(),
                self.meta,
            )
            .await
            {
                tracing::error!(request_id=%bill.request_id, ?error, "WS turn usage persistence failed");
            }
        }
    }
}

async fn consume(
    session: &Session,
    work: &Work,
    bill: &RequestBilling,
    routed: &mut routing::Routed,
) -> Progress {
    let mut progress = Progress::default();
    let mut gone = session.output.closed.subscribe();
    let mut draining = None;
    let first_deadline = Deadline::now() + routed.first_event;
    // The ledger reservation expires after ten minutes. Keep a hard per-turn
    // ceiling below it, leaving room for the 30s usage drain and settlement.
    let configured = session
        .state
        .setting_cached("responses_ws_turn_timeout_secs")
        .await;
    let seconds = configured
        .as_ref()
        .as_ref()
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .unwrap_or(480)
        .min(480);
    let turn_deadline = Deadline::from_std(bill.started) + Duration::from_secs(seconds);
    let mut received = false;
    loop {
        // Use one observation for both decisions: a close between the check and
        // select must wake the watch, not disable it and wait the five-minute idle deadline.
        let disconnected = session.output.is_closed();
        if disconnected && draining.is_none() {
            draining = Some(Deadline::now() + Duration::from_secs(30));
        }
        let deadline = draining.unwrap_or_else(|| {
            let idle = if received {
                Deadline::now() + Duration::from_mins(5)
            } else {
                first_deadline
            };
            idle.min(turn_deadline)
        });
        let next = tokio::select! {
            _ = gone.changed(), if !disconnected => continue,
            () = tokio::time::sleep_until(deadline) => {
                if progress.error.is_none() {
                    let mut error = AppError::new(StatusCode::GATEWAY_TIMEOUT, codes::UPSTREAM_TIMEOUT);
                    if Deadline::now() >= turn_deadline { error = error.with_param("responses_ws_turn_timeout"); }
                    progress.fail(session, work, error);
                }
                progress.suppressed = true;
                if draining.is_some() {
                    if let Some(pin) = session.upstream.lock().await.as_ref() { pin.transport.close(); }
                    break;
                }
                draining = Some(Deadline::now() + Duration::from_secs(30));
                continue;
            }
            event = routed.handle.events.next() => event,
        };
        match next {
            Some(Ok(event @ ChatEvent::Data { .. })) => {
                received = true;
                progress.event(session, work, bill, routed, &event).await;
                if progress.terminal {
                    break;
                }
            }
            Some(Ok(ChatEvent::Done)) | None => break,
            Some(Err(super::super::UpstreamError::Status { status, body, .. })) => {
                if progress.error.is_none() {
                    let status_code = StatusCode::from_u16(status)
                        .ok()
                        .filter(|s| s.is_client_error() || s.is_server_error())
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                    let code = format!("upstream_status_{status}");
                    let failure = ForwardFailure {
                        reply: FailureReply::Upstream {
                            status: status_code.as_u16(),
                            body,
                        },
                        error_code: code.clone(),
                        upstream_status: i16::try_from(status).ok(),
                        failover_count: routed.failover,
                        channel: Some((routed.info.channel, routed.info.key)),
                        upstream: None,
                    };
                    if !progress.suppressed {
                        report(session, work, &failure);
                    }
                    progress.error = Some(AppError::new(status_code, &code));
                    progress.suppressed = true;
                }
                break;
            }
            Some(Err(error)) => {
                let status = if error.error_code() == codes::UPSTREAM_TIMEOUT {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                };
                progress.fail(session, work, AppError::new(status, error.error_code()));
                break;
            }
        }
    }
    progress
}
