//! Responses WS gateway. Admission and settlement run per turn, never per socket.
mod bridge;
mod history;
mod output;
mod routing;
mod transport;
mod turn;

use super::{AppError, AppState, Bytes, HeaderMap, State, StatusCode, codes};
use axum::extract::ws::{
    Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use okapi_providers::responses_ws::{MAX_ACTIVE, MAX_NAMED_STREAMS};
use output::Output;
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::Instant;
use uuid::Uuid;

const MAX_MESSAGE: usize = 64 * 1024 * 1024;
const MAX_BUFFER: usize = 128 * 1024 * 1024;
const MAX_PENDING: usize = 64;
type Lane = Option<String>;

struct Lease {
    state: AppState,
    key: i64,
    connection: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let state = self.state.clone();
        let key = self.key;
        let connection = self.connection.clone();
        tokio::spawn(async move {
            state.sched.responses_ws_release(key, &connection).await;
        });
    }
}

pub async fn upgrade(
    State(state): State<AppState>,
    mut headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let connection = Uuid::new_v4();
    if !headers.contains_key("authorization") {
        let token = headers
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.split(',')
                    .map(str::trim)
                    .find_map(|v| v.strip_prefix("openai-insecure-api-key."))
            })
            .map(str::to_owned);
        if let Some(token) = token.and_then(|v| format!("Bearer {v}").parse().ok()) {
            headers.insert("authorization", token);
        }
    }
    let key = match crate::gateway::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_response_with(Some(connection)),
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(error) => {
            return AppError::new(error.into_response().status(), codes::BAD_REQUEST)
                .with_param("websocket")
                .into_response_with(Some(connection));
        }
    };
    let limit = state
        .setting_cached("responses_ws_max_conns_per_key")
        .await
        .as_ref()
        .as_ref()
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(4);
    // Own cleanup before awaiting the acquisition, including cancelled upgrades.
    let lease = Arc::new(Lease {
        state: state.clone(),
        key: key.key_id,
        connection: connection.to_string(),
    });
    let acquired = state
        .sched
        .responses_ws_acquire(key.key_id, &lease.connection, limit)
        .await;
    match acquired {
        Ok(true) => {}
        Ok(false) => {
            return AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("responses_ws_conns")
                .into_response_with(Some(connection));
        }
        Err(error) => return error.into_response_with(Some(connection)),
    }
    let principal = (key.user_id, key.key_id);
    super::with_request_id(
        upgrade
            .max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |socket| run(state, headers, principal, lease, socket)),
        connection,
    )
}

struct Session {
    state: AppState,
    headers: HeaderMap,
    principal: (i64, i64),
    upstream: Mutex<Option<routing::Pinned>>,
    history: Mutex<history::History>,
    replay_bytes: Arc<Semaphore>,
    output: Output,
    lease: Arc<Lease>,
}

struct Work {
    request: Uuid,
    lane: Lane,
    body: Bytes,
    warmup: bool,
    _slot: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

struct Queue {
    pending: VecDeque<Work>,
    active: HashSet<Lane>,
    named: HashSet<String>,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

impl Queue {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            active: HashSet::new(),
            named: HashSet::new(),
            slots: Arc::new(Semaphore::new(MAX_PENDING)),
            bytes: Arc::new(Semaphore::new(MAX_BUFFER)),
        }
    }

    fn enqueue(&mut self, raw: &str) -> Result<(), AppError> {
        let value: Value = serde_json::from_str(raw).map_err(|_| AppError::bad_request())?;
        if value.get("type").and_then(Value::as_str) != Some("response.create")
            || value.get("stream").is_some()
            || value.get("background").is_some()
            || value.get("generate").is_some_and(|v| !v.is_boolean())
        {
            return Err(AppError::bad_request().with_param("response.create"));
        }
        let lane = match value.get("stream_id") {
            None => None,
            Some(Value::String(name))
                if !name.is_empty()
                    && name.len() <= 256
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)) =>
            {
                Some(name.clone())
            }
            _ => return Err(AppError::bad_request().with_param("stream_id")),
        };
        if let Some(name) = &lane
            && !self.named.contains(name)
            && self.named.len() >= MAX_NAMED_STREAMS
        {
            return Err(
                AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                    .with_param("websocket_stream_limit_reached"),
            );
        }
        let busy = || {
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("responses_ws_queue")
        };
        let slot = self.slots.clone().try_acquire_owned().map_err(|_| busy())?;
        let size = u32::try_from(raw.len()).map_err(|_| busy())?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(size)
            .map_err(|_| busy())?;
        if let Some(name) = &lane {
            self.named.insert(name.clone());
        }
        self.pending.push_back(Work {
            request: Uuid::new_v4(),
            lane,
            body: Bytes::copy_from_slice(raw.as_bytes()),
            warmup: value.get("generate") == Some(&Value::Bool(false)),
            _slot: slot,
            _bytes: bytes,
        });
        Ok(())
    }

    fn start(&mut self, session: &Arc<Session>, finished: &mpsc::Sender<Lane>) {
        while self.active.len() < MAX_ACTIVE && !session.output.is_closed() {
            let Some(index) = self
                .pending
                .iter()
                .position(|w| !self.active.contains(&w.lane))
            else {
                break;
            };
            let Some(work) = self.pending.remove(index) else {
                break;
            };
            self.active.insert(work.lane.clone());
            let session = Arc::clone(session);
            let finished = finished.clone();
            let pending = session.state.settlements.clone();
            pending.spawn(async move {
                let lane = work.lane.clone();
                turn::run(&session, &work).await;
                // Keep the work permits until the complete settlement finishes.
                drop(work);
                let _ = finished.send(lane).await;
            });
        }
    }
}

async fn run(
    state: AppState,
    headers: HeaderMap,
    principal: (i64, i64),
    lease: Arc<Lease>,
    socket: WebSocket,
) {
    let (mut writer, mut reader) = socket.split();
    let (output, mut events) = Output::new();
    let mut gone = output.closed.subscribe();
    let write_output = output.clone();
    tokio::spawn(async move {
        loop {
            if write_output.is_closed() {
                break;
            }
            let event = tokio::select! {
                _ = gone.changed() => break,
                event = events.recv() => match event { Some(event) => event, None => break },
            };
            if !matches!(
                tokio::time::timeout(Duration::from_secs(10), writer.send(event.message)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        write_output.close();
        let _ = tokio::time::timeout(Duration::from_secs(1), writer.close()).await;
    });
    let context_limit = state.setting_cached("responses_ws_context_bytes").await;
    let context_limit = context_limit
        .as_ref()
        .as_ref()
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(MAX_BUFFER)
        .min(MAX_BUFFER);
    let session = Arc::new(Session {
        state,
        headers,
        principal,
        upstream: Mutex::new(None),
        history: Mutex::new(history::History::new(context_limit)),
        replay_bytes: Arc::new(Semaphore::new(MAX_BUFFER)),
        output,
        lease,
    });
    let mut gone = session.output.closed.subscribe();
    let mut activity = session.output.activity.subscribe();
    let mut queue = Queue::new();
    let (finished, mut completions) = mpsc::channel(MAX_ACTIVE);
    let mut renewal = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    let lifetime = Instant::now() + Duration::from_hours(1);
    let mut received = false;
    loop {
        if session.output.is_closed() {
            break;
        }
        queue.start(&session, &finished);
        let idle = *activity.borrow()
            + if received {
                Duration::from_mins(5)
            } else {
                Duration::from_secs(30)
            };
        tokio::select! {
            _ = gone.changed() => break,
            _ = activity.changed() => {},
            () = tokio::time::sleep_until(lifetime.min(idle)) => break,
            _ = renewal.tick() => {
                let valid = crate::gateway::auth::authenticate_data_plane(&session.state, &session.headers).await
                    .is_ok_and(|k| (k.user_id, k.key_id) == principal);
                if !valid || !session.state.sched.responses_ws_renew(principal.1, &session.lease.connection).await.unwrap_or(false) { break; }
            }
            Some(lane) = completions.recv() => { queue.active.remove(&lane); },
            event = reader.next() => match event {
                Some(Ok(Message::Text(raw))) => {
                    received = true;
                    session.output.touch();
                    if let Err(error) = queue.enqueue(raw.as_str()) {
                        let lane = serde_json::from_str::<Value>(raw.as_str()).ok()
                            .and_then(|v| v.get("stream_id").and_then(Value::as_str).map(str::to_owned))
                            .filter(|name| !name.is_empty() && name.len() <= 256 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)));
                        session.output.error(&error, Uuid::new_v4(), lane);
                    }
                }
                Some(Ok(Message::Ping(data))) => { session.output.push(Message::Pong(data)); }
                Some(Ok(Message::Pong(_))) => {},
                Some(Ok(Message::Binary(_))) => session.output.error(&AppError::bad_request().with_param("text_frame"), Uuid::new_v4(), None),
                _ => break,
            }
        }
    }
    session.output.close();
    // Pending frames were never admitted or charged. Active turns retain the
    // session/upstream and drain terminal usage independently of the client.
}
