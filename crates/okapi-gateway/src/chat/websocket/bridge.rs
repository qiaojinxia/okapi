use super::super::{
    BillingState, ChannelCandidate, Money, RequestBilling, StreamHandle, TokenUsage,
    record_terminal,
};
use super::{
    AppError, Bytes, Session, Work,
    history::{self, Capture, Context},
    transport,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub(super) struct Bridge {
    pub capture: Capture,
    pub request_id: transport::RequestId,
    pub local_warmup: bool,
}

pub(super) async fn start(
    session: &Session,
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: &Bytes,
    context: Option<&Context>,
    work: &Work,
) -> Result<(StreamHandle, Bridge), AppError> {
    let (mut value, input, root) = history::expand(body, context)?;
    let object = value.as_object_mut().ok_or_else(AppError::bad_request)?;
    for name in ["type", "stream_id", "generate"] {
        object.remove(name);
    }
    object.insert("stream".into(), true.into());
    let body = value.to_string();
    if body.len() > super::MAX_MESSAGE {
        return Err(context_limit());
    }
    let request = session
        .replay_bytes
        .clone()
        .try_acquire_many_owned(u32::try_from(body.len()).map_err(|_| context_limit())?)
        .map_err(|_| context_limit())?;
    let budget = session.history.lock().await.budget.clone();
    let capture = Capture::new(input, root, budget, request)?;
    let request_id = Arc::new(Mutex::new(None));
    let handle = if work.warmup {
        warmup(cand.upstream_model(&bill.model), work.lane.as_deref())
    } else {
        transport::http(
            bill.clone(),
            cand.clone(),
            Bytes::from(body),
            session.output.clone(),
            request_id.clone(),
            work.lane.clone(),
        )
    };
    Ok((
        handle,
        Bridge {
            capture,
            request_id,
            local_warmup: work.warmup,
        },
    ))
}

fn context_limit() -> AppError {
    AppError::new(
        super::StatusCode::PAYLOAD_TOO_LARGE,
        super::codes::BAD_REQUEST,
    )
    .with_param("responses_ws_context_limit")
}

fn warmup(model: &str, lane: Option<&str>) -> StreamHandle {
    let id = format!("{}{}", history::LOCAL_PREFIX, Uuid::new_v4().simple());
    let response = json!({"id":id,"object":"response","model":model,"status":"completed","output":[],"store":false,
        "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0},"okapi_warmup":"local"});
    let mut events = Vec::new();
    for kind in ["response.created", "response.completed"] {
        let mut value = json!({"type":kind,"response":response});
        if kind == "response.created" {
            value["response"]["status"] = "in_progress".into();
        }
        if let Some(lane) = lane {
            value["stream_id"] = lane.into();
        }
        events.extend(
            okapi_providers::responses::parse_event(kind, &value.to_string())
                .into_iter()
                .map(Ok),
        );
    }
    StreamHandle {
        upstream_request_id: None,
        events: Box::pin(futures::stream::iter(events)),
    }
}

/// Local preparation never invokes the model, including on flat-price models.
pub(super) async fn settle_warmup(bill: &RequestBilling, info: &super::super::CandInfo) {
    record_terminal(
        bill,
        info,
        TokenUsage::default(),
        Money::ZERO,
        None,
        BillingState::Committed,
        2,
        None,
        None,
        0,
        "commit",
        0,
        bill.reservation_pool,
    )
    .await;
}

pub(super) fn previous(work: &Work) -> Option<String> {
    serde_json::from_slice::<Value>(&work.body)
        .ok()?
        .get("previous_response_id")?
        .as_str()
        .map(str::to_owned)
}
