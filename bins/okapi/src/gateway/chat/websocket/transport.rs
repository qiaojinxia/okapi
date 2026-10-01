use super::super::{AppError, ChannelCandidate, RequestBilling, StreamHandle, UpstreamError};
use super::{Bytes, Session, Value, output::Output};
use crate::gateway::oauth_cred;
use futures::{StreamExt, TryStreamExt};
use okapi_providers::{
    Outbound,
    oauth::codex,
    responses_ws::{ResponsesSocket, SocketTimeouts},
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) enum Transport {
    Native(ResponsesSocket),
    Http,
}
impl Transport {
    pub fn close(&self) {
        if let Self::Native(socket) = self {
            socket.close();
        }
    }
    pub fn closed(&self) -> bool {
        matches!(self, Self::Native(socket) if socket.is_closed())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Policy {
    Native,
    Http,
    Auto,
}
impl Policy {
    pub async fn load(session: &Session, channel: i64) -> Result<Self, AppError> {
        // Keep channel overrides live, just like the fresh candidate eligibility query.
        let channel: Option<Value> = sqlx::query_scalar(
            "SELECT settings -> 'responses_ws_transport' FROM channels WHERE id=$1",
        )
        .bind(channel)
        .fetch_optional(&session.state.pg)
        .await
        .map_err(|e| AppError::from(okapi_store::StoreError::from(e)))?
        .flatten();
        let global = session.state.setting_cached("responses_ws_transport").await;
        match channel
            .as_ref()
            .filter(|v| !v.is_null())
            .or(global.as_ref().as_ref())
        {
            None | Some(Value::Null) => Ok(Self::Auto),
            Some(Value::String(value)) => match value.as_str() {
                "native" => Ok(Self::Native),
                "http" => Ok(Self::Http),
                "auto" => Ok(Self::Auto),
                _ => Err(AppError::bad_request().with_param("responses_ws_transport")),
            },
            _ => Err(AppError::bad_request().with_param("responses_ws_transport")),
        }
    }
    pub fn allows(self, transport: &Transport, cand: &ChannelCandidate) -> bool {
        let enabled = |name| cand.capabilities.get(name).and_then(Value::as_bool) != Some(false);
        match transport {
            Transport::Native(_) => self != Self::Http && enabled("responses_websocket"),
            Transport::Http => self != Self::Native && enabled("responses_http"),
        }
    }
    pub fn prefer_http(self, cand: &ChannelCandidate) -> bool {
        self == Self::Http
            || (self == Self::Auto
                && cand
                    .capabilities
                    .get("responses_websocket")
                    .and_then(Value::as_bool)
                    == Some(false))
    }
}

struct Credentials {
    headers: Vec<(String, String)>,
    outbound: Outbound,
    url: String,
}
async fn credentials(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
) -> Result<Credentials, UpstreamError> {
    let outbound = oauth_cred::outbound_with_client(cand, &bill.client_headers);
    let credential = if cand.provider == "codex" {
        Some(oauth_cred::fresh_credential(&bill.state, cand).await?)
    } else {
        None
    };
    let token = credential
        .as_ref()
        .map_or(cand.credential.as_str(), |c| c.access_token.as_str());
    let mut headers = vec![("authorization".into(), format!("Bearer {token}"))];
    if let Some(credential) = credential {
        let has = |name: &str| {
            outbound
                .extra_headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case(name))
        };
        if !has("originator") {
            headers.push(("originator".into(), codex::ORIGINATOR.into()));
        }
        if !has("openai-beta") {
            headers.push(("openai-beta".into(), "responses=experimental".into()));
        }
        if let Some(account) = credential.account_id {
            headers.push(("chatgpt-account-id".into(), account));
        }
    }
    let base = cand
        .api_base
        .as_deref()
        .unwrap_or(if cand.provider == "codex" {
            codex::DEFAULT_API_BASE
        } else {
            super::super::DEFAULT_OPENAI_BASE
        });
    Ok(Credentials {
        headers,
        outbound,
        url: format!("{}/responses", base.trim_end_matches('/')),
    })
}

pub(super) async fn connect(
    bill: &RequestBilling,
    cand: &mut ChannelCandidate,
) -> Result<Transport, UpstreamError> {
    if cand.provider == "codex" {
        cand.credential = oauth_cred::fresh_credential(&bill.state, cand)
            .await?
            .to_plaintext();
    }
    let first = connect_once(bill, cand).await;
    if cand.provider == "codex" && matches!(&first, Err(UpstreamError::Status { status: 401, .. }))
    {
        match oauth_cred::refresh_rejected_credential(&bill.state, cand).await {
            Ok(credential) => {
                cand.credential = credential.to_plaintext();
                // The upgrade rejected authentication before any create frame was sent.
                return connect_once(bill, cand).await;
            }
            Err(error) => bill.trace.failure(&error),
        }
    }
    first
}

async fn connect_once(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
) -> Result<Transport, UpstreamError> {
    let credentials = credentials(bill, cand).await?;
    let headers: Vec<_> = credentials
        .headers
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    ResponsesSocket::connect(
        bill.state.upstream.http(),
        &credentials.url,
        &headers,
        &credentials.outbound,
        SocketTimeouts::default(),
    )
    .await
    .map(Transport::Native)
}

pub(super) type RequestId = Arc<Mutex<Option<String>>>;

pub(super) fn http(
    bill: RequestBilling,
    cand: ChannelCandidate,
    body: Bytes,
    output: Output,
    id: RequestId,
    lane: Option<String>,
) -> StreamHandle {
    // Defer the one POST into the stream owner. Downstream timeouts stop delivery
    // but keep polling this same request during the usage drain; they never resend it.
    let begin = futures::stream::once(async move {
        if output.is_closed() {
            return Err(disconnected());
        }
        let credentials = credentials(&bill, &cand).await?;
        if output.is_closed() {
            return Err(disconnected());
        }
        let headers: Vec<_> = credentials
            .headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let handle = okapi_providers::responses_bridge::send(
            bill.state.upstream.http(),
            credentials.url,
            &headers,
            body,
            &credentials.outbound,
        )
        .await?;
        if let Ok(mut id) = id.lock() {
            *id = handle.upstream_request_id;
        }
        Ok(handle.events)
    });
    let events = begin.try_flatten().map(move |event| {
        event.and_then(|event| okapi_providers::responses_bridge::lane(event, lane.as_deref()))
    });
    StreamHandle {
        upstream_request_id: None,
        events: Box::pin(events),
    }
}

fn disconnected() -> UpstreamError {
    UpstreamError::Session {
        reason: "client_disconnected",
        timed_out: false,
    }
}

pub(super) fn unsupported(error: &UpstreamError) -> bool {
    matches!(
        error,
        UpstreamError::Status {
            status: 404 | 405 | 426 | 501,
            ..
        }
    )
}
