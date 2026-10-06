use super::super::{
    AppError, AttemptError, Bytes, CandInfo, ChannelCandidate, ForwardFailure, Ingress,
    MAX_ATTEMPTS, ProbeInfo, RequestBilling, ResponseBinding, ResponseWriter, StatusCode,
    StreamHandle, UpstreamError, build_upstream_body, cand_info, classify_fatal, codes,
    eligible_candidates, first_output_window, shape_upstream_body,
};
use super::{
    Session, Work, bridge, history,
    transport::{self, Policy, Transport},
};
use crate::gateway::scheduler::CandidateSet;
use okapi_providers::{oauth::codex, responses_ws::ResponsesSocket};
use serde_json::Value;
use std::time::Duration;

pub(super) struct Pinned {
    pub transport: Transport,
    binding: ResponseBinding,
    /// 首轮选定的出口（代理 id + URL；None = 直连）。会话内后续轮次沿用，轮换组也不重抽：
    /// 同一个会话中途换出口，上游看到的就是同一账号的连接在两个 IP 之间跳。
    proxy_id: Option<i64>,
    proxy: Option<String>,
}

pub(super) struct Routed {
    pub handle: StreamHandle,
    pub info: CandInfo,
    pub writer: Option<ResponseWriter>,
    pub failover: i16,
    pub first_event: Duration,
    pub bridge: Option<bridge::Bridge>,
}

fn unavailable(param: &str) -> ForwardFailure {
    ForwardFailure::app(
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL)
            .with_param(param),
        0,
        None,
    )
}

fn failure(
    error: UpstreamError,
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    failover: i16,
) -> ForwardFailure {
    bill.trace.failure(&error);
    let channel = (cand.channel_id, cand.channel_key_id);
    // This conversion does not authorize retry. Only the handshake loop below may retry.
    match classify_fatal(error, failover, channel) {
        AttemptError::Fatal(failure) => failure,
        AttemptError::Retriable {
            code,
            upstream_status,
            ..
        } => {
            let mut failure = ForwardFailure::app(
                AppError::new(
                    if code == codes::NO_AVAILABLE_CHANNEL {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    code,
                ),
                failover,
                Some(channel),
            );
            failure.upstream_status = upstream_status;
            failure
        }
    }
}

async fn slot(bill: &RequestBilling, cand: &ChannelCandidate) -> bool {
    if let Some(limit) = cand.rpm_limit
        && !bill
            .state
            .sched
            .channel_key_rate_ok(cand.channel_key_id, i64::from(limit))
            .await
    {
        return false;
    }
    if let Some(cap) = cand.daily_spend_cap_micro
        && bill
            .state
            .sched
            .channel_key_spend_get(cand.channel_key_id)
            .await
            >= cap
    {
        return false;
    }
    true
}

fn body(
    bill: &RequestBilling,
    probe: &ProbeInfo,
    cand: &ChannelCandidate,
    work: &Work,
) -> Result<Bytes, UpstreamError> {
    // 与 HTTP 同一规则：没写输出上限时按预扣封顶补上（Responses 原文，转换前）
    let bounded = super::super::bound_default_output(
        crate::gateway::ingress::Ingress::Responses,
        bill.default_output_cap,
        work.body.clone(),
    );
    let built = build_upstream_body(
        bill,
        probe,
        cand,
        &bounded,
        cand.upstream_model(&bill.model),
    )?;
    let shaped = shape_upstream_body(bill, cand, built)?;
    let original: Value =
        serde_json::from_slice(&work.body).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let mut value: Value =
        serde_json::from_slice(&shaped).map_err(|e| UpstreamError::Build(e.to_string()))?;
    // Channel customization cannot turn a warmup into generation, alter lanes or inject history.
    for field in [
        "type",
        "stream_id",
        "generate",
        "store",
        "previous_response_id",
        "conversation",
        "stream",
        "background",
    ] {
        if original.get(field) != value.get(field) {
            return Err(UpstreamError::Build(format!(
                "responses_ws_protected_field:{field}"
            )));
        }
    }
    if cand.provider == "codex" {
        value = serde_json::from_slice(&codex::prepare_body(&shaped)?)
            .map_err(|e| UpstreamError::Build(e.to_string()))?;
        value
            .as_object_mut()
            .ok_or_else(|| UpstreamError::Build("body_not_object".into()))?
            .remove("stream");
    }
    Ok(Bytes::from(value.to_string()))
}

// Keep slot ownership, protocol selection and the no-replay boundary in one place.
#[allow(clippy::too_many_lines)]
pub(super) async fn route(
    session: &Session,
    bill: &RequestBilling,
    probe: &ProbeInfo,
    work: &Work,
    context: Option<&history::Context>,
) -> Result<Routed, ForwardFailure> {
    let mut pinned = session.upstream.lock().await;
    let (mut candidates, sticky) = eligible_candidates(bill, probe, true).await?;
    candidates.retain(|c| {
        crate::gateway::execution_plan::ExecutionPlan::compile(
            crate::gateway::ingress::Ingress::Responses,
            c,
            &bill.model,
            crate::gateway::execution_plan::Requirements::default(),
        )
        .is_ok_and(crate::gateway::execution_plan::ExecutionPlan::websocket_ingress)
    });
    if let Some(pin) = pinned.as_ref() {
        if pin.transport.closed() {
            return Err(unavailable("responses_ws_closed"));
        }
        candidates.retain(|c| pin.binding.matches(c) && c.egress_admits(pin.proxy_id));
    }
    let mut last = unavailable("responses_websocket");
    let mut attempted: i16 = 0;
    for mut cand in candidates {
        if usize::try_from(attempted).unwrap_or(MAX_ATTEMPTS) >= MAX_ATTEMPTS {
            break;
        }
        if session.output.is_closed() {
            return Err(unavailable("client_disconnected"));
        }
        let policy = Policy::load(session, cand.channel_id)
            .await
            .map_err(|error| {
                ForwardFailure::app(
                    error,
                    attempted,
                    Some((cand.channel_id, cand.channel_key_id)),
                )
            })?;
        if let Some(pin) = pinned.as_ref() {
            cand.pin_egress(pin.proxy_id, pin.proxy.clone());
            if !policy.allows(&pin.transport, &cand) {
                continue;
            }
        } else if policy.prefer_http(&cand) {
            if !policy.allows(&Transport::Http, &cand) {
                continue;
            }
        } else if cand
            .capabilities
            .get("responses_websocket")
            .and_then(Value::as_bool)
            == Some(false)
        {
            continue;
        }
        if !slot(bill, &cand).await {
            continue;
        }
        let permit = crate::gateway::sched_redis::channel_permit::ChannelPermit::acquire(
            &bill.state.sched,
            &cand,
        )
        .await
        .map_err(|error| failure(error, bill, &cand, attempted))?;
        let Some(permit) = permit else {
            continue;
        };
        let failover = attempted;
        attempted += 1;
        bill.trace
            .begin(&cand, cand.upstream_model(&bill.model), "/v1/responses");
        let body = match body(bill, probe, &cand, work) {
            Ok(body) => body,
            Err(error) => {
                return Err(failure(error, bill, &cand, failover));
            }
        };
        if pinned.is_none() {
            let selected = if policy.prefer_http(&cand) {
                Ok(Transport::Http)
            } else {
                transport::connect(bill, &mut cand).await
            };
            let selected = match selected {
                Ok(selected) => selected,
                Err(ref error)
                    if policy == Policy::Auto
                        && transport::unsupported(error)
                        && policy.allows(&Transport::Http, &cand) =>
                {
                    Transport::Http
                }
                Err(error) => {
                    let retry = error.retriable_before_first_token();
                    let mut kind = super::super::failure_kind_of(&error);
                    if cand.provider == "codex" {
                        match &error {
                            UpstreamError::Status { status: 401, .. } => {
                                kind = okapi_store::channels::KeyFailure::RateLimited {
                                    retry_after_secs: Some(30),
                                };
                            }
                            UpstreamError::Status {
                                status: 429,
                                retry_after_secs,
                                ..
                            } => {
                                if matches!(
                                    kind,
                                    okapi_store::channels::KeyFailure::RateLimited { .. }
                                ) {
                                    kind = okapi_store::channels::KeyFailure::RateLimited {
                                        retry_after_secs: Some(retry_after_secs.unwrap_or(5)),
                                    };
                                }
                            }
                            _ => {}
                        }
                    }
                    last = failure(error, bill, &cand, failover);
                    if retry {
                        crate::gateway::key_health::failure(
                            &bill.state,
                            &cand,
                            &last.error_code,
                            kind,
                        )
                        .await;
                    }
                    if !retry || !bill.prefs.allow_fallbacks || bill.response_parent.is_some() {
                        return Err(last);
                    }
                    continue;
                }
            };
            if matches!(selected, Transport::Native(_)) {
                // 上游接受了这把凭证的 WS 握手：key 是好的
                crate::gateway::key_health::success(&bill.state, &cand).await;
            }
            *pinned = Some(Pinned {
                transport: selected,
                binding: ResponseBinding::from_candidate(&cand),
                proxy_id: cand.egress_proxy_id,
                proxy: cand.proxy_url.clone(),
            });
        }
        let selected = pinned.as_ref().expect("selected above").transport.clone();
        // Once selected, hold the account but release admission before any HTTP POST.
        drop(pinned);
        if session.output.is_closed() {
            return Err(unavailable("client_disconnected"));
        }
        match selected {
            Transport::Native(socket) => {
                return create_turn(&socket, body, bill, &cand, sticky, failover, permit).await;
            }
            Transport::Http => {
                permit.release().await;
                let (handle, bridge) =
                    match bridge::start(session, bill, &cand, &body, context, work).await {
                        Ok(result) => result,
                        Err(error) => {
                            return Err(ForwardFailure::app(
                                error,
                                failover,
                                Some((cand.channel_id, cand.channel_key_id)),
                            ));
                        }
                    };
                bill.trace.finish(None, None);
                let mut info = cand_info(
                    &cand,
                    &bill.model,
                    true,
                    Ingress::Responses,
                    if bill.response_parent.is_some() { 1 } else { 3 },
                    0,
                );
                if bridge.local_warmup {
                    info.upstream_endpoint.clear();
                }
                return Ok(Routed {
                    handle,
                    info,
                    writer: Some(ResponseWriter::new(ResponseBinding::from_candidate(&cand))),
                    failover,
                    first_event: first_output_window(&cand),
                    bridge: Some(bridge),
                });
            }
        }
    }
    Err(last)
}

async fn create_turn(
    socket: &ResponsesSocket,
    body: Bytes,
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    sticky: Option<i64>,
    failover: i16,
    permit: crate::gateway::sched_redis::channel_permit::ChannelPermit,
) -> Result<Routed, ForwardFailure> {
    // create may already have sent a frame when it fails. Never replay or switch accounts here.
    let result = match crate::gateway::account_control::admit(
        &bill.state,
        cand.channel_id,
        Some(cand.channel_key_id),
    )
    .await
    {
        Ok(()) => socket.create(body).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(mut handle) => {
            handle.events = okapi_providers::response_lifetime::guard_stream(handle.events, permit);
            bill.trace.finish(None, None);
            let layer = if bill.response_parent.is_some() {
                1
            } else if sticky == Some(cand.channel_key_id) {
                2
            } else {
                3
            };
            let mut info = cand_info(cand, &bill.model, true, Ingress::Responses, layer, 0);
            info.upstream_request_id
                .clone_from(&handle.upstream_request_id);
            Ok(Routed {
                handle,
                info,
                writer: Some(ResponseWriter::new(ResponseBinding::from_candidate(cand))),
                failover,
                first_event: first_output_window(cand),
                bridge: None,
            })
        }
        Err(error) => Err(failure(error, bill, cand, failover)),
    }
}
