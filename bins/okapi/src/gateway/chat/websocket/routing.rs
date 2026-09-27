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
use okapi_providers::{oauth::codex, responses_ws::ResponsesSocket};
use serde_json::Value;
use std::time::Duration;

pub(super) struct Pinned {
    pub transport: Transport,
    binding: ResponseBinding,
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

fn failure(error: UpstreamError, cand: &ChannelCandidate, failover: i16) -> ForwardFailure {
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
                AppError::new(StatusCode::BAD_GATEWAY, code),
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
    bill.state
        .sched
        .acquire_slot(cand.channel_key_id, cand.max_concurrency)
        .await
}

fn body(
    bill: &RequestBilling,
    probe: &ProbeInfo,
    cand: &ChannelCandidate,
    work: &Work,
) -> Result<Bytes, UpstreamError> {
    let built = build_upstream_body(
        bill,
        probe,
        cand,
        &work.body,
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
    candidates.retain(|c| c.responses_native && matches!(c.provider.as_str(), "openai" | "codex"));
    if let Some(pin) = pinned.as_ref() {
        if pin.transport.closed() {
            return Err(unavailable("responses_ws_closed"));
        }
        candidates.retain(|c| pin.binding.matches(c) && pin.proxy == c.proxy_url);
    }
    let mut last = unavailable("responses_websocket");
    let mut attempted: i16 = 0;
    for cand in candidates {
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
        let failover = attempted;
        attempted += 1;
        let body = match body(bill, probe, &cand, work) {
            Ok(body) => body,
            Err(error) => {
                bill.state
                    .sched
                    .release_slot(cand.channel_key_id, cand.max_concurrency)
                    .await;
                return Err(failure(error, &cand, failover));
            }
        };
        if pinned.is_none() {
            let selected = if policy.prefer_http(&cand) {
                Ok(Transport::Http)
            } else {
                transport::connect(bill, &cand).await
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
                    let kind = super::super::failure_kind_of(&error);
                    last = failure(error, &cand, failover);
                    bill.state
                        .sched
                        .release_slot(cand.channel_key_id, cand.max_concurrency)
                        .await;
                    if retry {
                        let _ = okapi_store::channels::mark_key_failure(
                            &bill.state.pg,
                            cand.channel_key_id,
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
            *pinned = Some(Pinned {
                transport: selected,
                binding: ResponseBinding::from_candidate(&cand),
                proxy: cand.proxy_url.clone(),
            });
        }
        let selected = pinned.as_ref().expect("selected above").transport.clone();
        // Once selected, hold the account but release admission before any HTTP POST.
        drop(pinned);
        if session.output.is_closed() {
            bill.state
                .sched
                .release_slot(cand.channel_key_id, cand.max_concurrency)
                .await;
            return Err(unavailable("client_disconnected"));
        }
        match selected {
            Transport::Native(socket) => {
                return create_turn(&socket, body, bill, &cand, sticky, failover).await;
            }
            Transport::Http => {
                let (handle, bridge) =
                    match bridge::start(session, bill, &cand, &body, context, work).await {
                        Ok(result) => result,
                        Err(error) => {
                            bill.state
                                .sched
                                .release_slot(cand.channel_key_id, cand.max_concurrency)
                                .await;
                            return Err(ForwardFailure::app(
                                error,
                                failover,
                                Some((cand.channel_id, cand.channel_key_id)),
                            ));
                        }
                    };
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
) -> Result<Routed, ForwardFailure> {
    // create may already have sent a frame when it fails. Never replay or switch accounts here.
    match socket.create(body).await {
        Ok(handle) => {
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
        Err(error) => {
            bill.state
                .sched
                .release_slot(cand.channel_key_id, cand.max_concurrency)
                .await;
            Err(failure(error, cand, failover))
        }
    }
}
