use super::{
    BUFFER_BYTES, BufferedEvents, Failure, Lane, MAX_ACTIVE, MAX_NAMED_STREAMS, SocketTimeouts,
    Turn, handshake::Socket,
};
use crate::ChatEvent;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Semaphore, mpsc, watch},
    time::{Instant, timeout},
};
use tokio_tungstenite::tungstenite::Message;

struct Driver {
    active: HashMap<Lane, Turn>,
    queued: VecDeque<Turn>,
    named: HashSet<String>,
    latest: HashMap<Lane, String>,
    bytes: Arc<Semaphore>,
}

pub(super) async fn run(
    mut socket: Socket,
    mut commands: mpsc::Receiver<Turn>,
    mut closed: watch::Receiver<bool>,
    timeouts: SocketTimeouts,
) {
    let mut driver = Driver {
        active: HashMap::new(),
        queued: VecDeque::new(),
        named: HashSet::new(),
        latest: HashMap::new(),
        bytes: Arc::new(Semaphore::new(BUFFER_BYTES)),
    };
    let lifetime = Instant::now() + timeouts.lifetime.min(Duration::from_hours(1));
    let mut interrupt = closed.clone();
    // Close/lifetime also interrupt a blocked write or a full consumer buffer.
    let failure = tokio::select! {
        failure = driver.run(&mut socket, &mut commands, &mut closed, timeouts, lifetime) => failure,
        _ = interrupt.changed() => Failure::Stream("responses_ws_closed"),
        () = tokio::time::sleep_until(lifetime) => Failure::Stream("websocket_connection_limit_reached"),
    };
    commands.close();
    while let Ok(turn) = commands.try_recv() {
        driver.queued.push_back(turn);
    }
    for turn in driver.active.into_values().chain(driver.queued) {
        let _ = turn.finish.send(Err(failure));
    }
    // All streams are notified even when the peer never completes a close handshake.
    let _ = timeout(timeouts.io, socket.close(None)).await;
}

impl Driver {
    async fn run(
        &mut self,
        socket: &mut Socket,
        commands: &mut mpsc::Receiver<Turn>,
        closed: &mut watch::Receiver<bool>,
        timeouts: SocketTimeouts,
        lifetime: Instant,
    ) -> Failure {
        let mut idle = Instant::now() + timeouts.idle;
        loop {
            if *closed.borrow() {
                return Failure::Stream("responses_ws_closed");
            }
            match self.start_ready(socket, timeouts.io).await {
                Err(failure) => return failure,
                Ok(true) => idle = Instant::now() + timeouts.idle,
                Ok(false) => {}
            }
            tokio::select! {
                () = tokio::time::sleep_until(lifetime) => return Failure::Stream("websocket_connection_limit_reached"),
                () = tokio::time::sleep_until(idle) => return Failure::Timeout,
                _ = closed.changed() => return Failure::Stream("responses_ws_closed"),
                command = commands.recv() => match command {
                    Some(turn) => self.admit(turn),
                    None => return Failure::Stream("responses_ws_closed"),
                },
                incoming = socket.next() => {
                    idle = Instant::now() + timeouts.idle;
                    match incoming {
                        Some(Ok(Message::Text(raw))) => {
                            if let Err(failure) = self.event(raw.as_str(), timeouts.io).await { return failure; }
                        }
                        Some(Ok(Message::Ping(_))) => {
                            // tungstenite automatically enqueues the corresponding pong.
                            if !matches!(timeout(timeouts.io, socket.flush()).await, Ok(Ok(()))) { return Failure::Stream("responses_ws_ping_failed"); }
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Binary(_) | Message::Frame(_))) => return Failure::Stream("responses_ws_non_text_event"),
                        Some(Ok(Message::Close(_))) | None => return Failure::Stream("responses_ws_closed"),
                        Some(Err(_)) => return Failure::Stream("responses_ws_transport"),
                    }
                }
            }
        }
    }

    fn admit(&mut self, mut turn: Turn) {
        let Some(admission) = turn.admission.take() else {
            return;
        };
        if admission.is_closed() {
            return;
        }
        if let Some(name) = &turn.lane.0 {
            if !self.named.contains(name) && self.named.len() >= MAX_NAMED_STREAMS {
                let _ = admission.send(Err("websocket_stream_limit_reached"));
                return;
            }
            self.named.insert(name.clone());
        }
        if admission.send(Ok(())).is_ok() {
            self.queued.push_back(turn);
        }
    }

    async fn start_ready(
        &mut self,
        socket: &mut Socket,
        deadline: Duration,
    ) -> Result<bool, Failure> {
        let mut started = false;
        while self.active.len() < MAX_ACTIVE {
            let Some(index) = self
                .queued
                .iter()
                .position(|turn| !self.active.contains_key(&turn.lane))
            else {
                break;
            };
            let Some(mut turn) = self.queued.remove(index) else {
                break;
            };
            if turn.events.is_closed() {
                let _ = turn.finish.send(Ok(()));
                continue;
            }
            let raw = String::from_utf8(turn.body.to_vec())
                .map_err(|_| Failure::Stream("responses_ws_invalid_json"))?;
            let lane = turn.lane.clone();
            // Register before send: a partial write is an uncertain execution, never a retry signal here.
            turn.body = bytes::Bytes::new();
            self.active.insert(lane.clone(), turn);
            started = true;
            timeout(deadline, socket.send(Message::Text(raw.into())))
                .await
                .map_err(|_| Failure::Timeout)?
                .map_err(|_| Failure::Stream("responses_ws_write_failed"))?;
            if let Some(turn) = self.active.get_mut(&lane) {
                turn.bytes = None;
            }
        }
        Ok(started)
    }

    async fn event(&mut self, raw: &str, deadline: Duration) -> Result<(), Failure> {
        let value: Value =
            serde_json::from_str(raw).map_err(|_| Failure::Stream("responses_ws_invalid_event"))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or(Failure::Stream("responses_ws_invalid_event"))?;
        if value.pointer("/error/code").and_then(Value::as_str)
            == Some("websocket_connection_limit_reached")
        {
            return Err(Failure::Stream("websocket_connection_limit_reached"));
        }
        let lane =
            Lane::read(&value).map_err(|_| Failure::Stream("responses_ws_invalid_stream_id"))?;
        let turn = self
            .active
            .get_mut(&lane)
            .ok_or(Failure::Stream("responses_ws_unknown_stream"))?;
        let terminal = matches!(
            kind,
            "response.completed" | "response.incomplete" | "response.failed" | "error"
        );
        let id_value = value
            .pointer("/response/id")
            .or_else(|| value.get("response_id"));
        let id = id_value.and_then(Value::as_str);
        if id_value.is_some() && id.is_none() {
            return Err(Failure::Stream("responses_ws_response_id_mismatch"));
        }
        if let Some(id) = id {
            if id.is_empty()
                || id.len() > 512
                || turn.response_id.as_deref().is_some_and(|known| known != id)
                || self.latest.get(&lane).is_some_and(|old| old == id)
            {
                return Err(Failure::Stream("responses_ws_response_id_mismatch"));
            }
            turn.response_id = Some(id.to_owned());
        }
        if terminal && kind != "error" && id.is_none() {
            return Err(Failure::Stream("responses_ws_missing_response_id"));
        }
        let mut events = crate::responses::parse_event(kind, raw);
        if kind == "error" {
            events.push(ChatEvent::Done);
        }
        if !turn.events.is_closed() {
            let bytes = timeout(
                deadline,
                self.bytes.clone().acquire_many_owned(
                    u32::try_from(raw.len())
                        .map_err(|_| Failure::Stream("responses_ws_message_too_large"))?,
                ),
            )
            .await
            .map_err(|_| Failure::Stream("responses_ws_backpressure"))?
            .map_err(|_| Failure::Stream("responses_ws_closed"))?;
            // On overflow close the connection rather than silently drop usage or output.
            if timeout(
                deadline,
                turn.events.send(BufferedEvents {
                    events: events.into(),
                    _bytes: bytes,
                }),
            )
            .await
            .is_err()
            {
                return Err(Failure::Stream("responses_ws_backpressure"));
            }
        }
        if terminal && let Some(turn) = self.active.remove(&lane) {
            if let Some(id) = turn.response_id {
                self.latest.insert(lane, id);
            }
            let _ = turn.finish.send(Ok(()));
        }
        Ok(())
    }
}
