//! 原生 Responses 持久连接。每个实例只属于一个下游会话与一个上游账号。
//!
//! 此层不做鉴权、选路、上下文重放或计费；gateway 必须逐轮执行这些步骤。
//! 同 lane 串行、跨 lane 并发；异常断开不重连、不重放已发送请求。
//! 丢弃某轮接收器只停止交付，仍排空该轮上游事件，不伪造取消指令。

mod driver;
mod handshake;

use crate::{ChatEvent, HttpPool, Outbound, StreamHandle, UpstreamError};
use bytes::Bytes;
use futures::stream;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

pub const MAX_ACTIVE: usize = 16;
pub const MAX_NAMED_STREAMS: usize = 32;
const MAX_PENDING: usize = 64;
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
const BUFFER_BYTES: usize = 128 * 1024 * 1024;

/// Transport deadlines; a session never exceeds the protocol's one-hour lifetime.
#[derive(Clone, Copy)]
pub struct SocketTimeouts {
    pub handshake: Duration,
    pub io: Duration,
    pub idle: Duration,
    pub lifetime: Duration,
}

impl Default for SocketTimeouts {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            io: Duration::from_secs(10),
            idle: Duration::from_mins(5),
            lifetime: Duration::from_hours(1),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Lane(Option<String>);

impl Lane {
    fn read(value: &Value) -> Result<Self, UpstreamError> {
        match value.get("stream_id") {
            None => Ok(Self(None)),
            Some(Value::String(name))
                if !name.is_empty()
                    && name.len() <= 256
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)) =>
            {
                Ok(Self(Some(name.clone())))
            }
            _ => Err(UpstreamError::Build("invalid_stream_id".into())),
        }
    }
}

#[derive(Clone, Copy)]
enum Failure {
    Timeout,
    Stream(&'static str),
}

impl Failure {
    fn error(self) -> UpstreamError {
        match self {
            Self::Timeout => UpstreamError::Session {
                reason: "responses_ws_timeout",
                timed_out: true,
            },
            Self::Stream(reason) => UpstreamError::Session {
                reason,
                timed_out: false,
            },
        }
    }
}

struct BufferedEvents {
    events: VecDeque<ChatEvent>,
    _bytes: OwnedSemaphorePermit,
}

struct Turn {
    lane: Lane,
    body: Bytes,
    response_id: Option<String>,
    events: mpsc::Sender<BufferedEvents>,
    finish: oneshot::Sender<Result<(), Failure>>,
    admission: Option<oneshot::Sender<Result<(), &'static str>>>,
    _slot: OwnedSemaphorePermit,
    bytes: Option<OwnedSemaphorePermit>,
}

/// Clones share one socket. Dropping the last clone or calling `close` aborts it.
/// A dropped per-turn stream does not close unrelated lanes. The billing owner
/// must retain/drain its stream if the downstream consumer disconnects.
#[derive(Clone)]
pub struct ResponsesSocket {
    commands: mpsc::Sender<Turn>,
    close: watch::Sender<bool>,
    slots: Arc<Semaphore>,
    request_bytes: Arc<Semaphore>,
    request_id: Option<String>,
}

impl ResponsesSocket {
    pub async fn connect(
        http: &HttpPool,
        url: &str,
        headers: &[(&str, &str)],
        outbound: &Outbound,
        timeouts: SocketTimeouts,
    ) -> Result<Self, UpstreamError> {
        let (socket, request_id) = tokio::time::timeout(
            timeouts.handshake,
            handshake::connect(http, url, headers, outbound),
        )
        .await
        .map_err(|_| UpstreamError::Timeout)??;
        let (commands, receiver) = mpsc::channel(MAX_PENDING);
        let (close, closed) = watch::channel(false);
        tokio::spawn(driver::run(socket, receiver, closed, timeouts));
        Ok(Self {
            commands,
            close,
            slots: Arc::new(Semaphore::new(MAX_PENDING)),
            request_bytes: Arc::new(Semaphore::new(BUFFER_BYTES)),
            request_id,
        })
    }

    /// Accept an already prepared response.create frame, preserving opaque input.
    /// Local admission errors are returned before anything is sent upstream.
    pub async fn create(&self, body: Bytes) -> Result<StreamHandle, UpstreamError> {
        if body.len() > MAX_MESSAGE {
            return Err(UpstreamError::Build(
                "responses_ws_message_too_large".into(),
            ));
        }
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| UpstreamError::Build("responses_ws_invalid_json".into()))?;
        if value.get("type").and_then(Value::as_str) != Some("response.create") {
            return Err(UpstreamError::Build("responses_ws_invalid_event".into()));
        }
        if value.get("stream").is_some() || value.get("background").is_some() {
            return Err(UpstreamError::Build("responses_ws_http_only_fields".into()));
        }
        let lane = Lane::read(&value)?;
        drop(value);
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| UpstreamError::Build("responses_ws_queue_full".into()))?;
        let bytes = self
            .request_bytes
            .clone()
            .try_acquire_many_owned(
                u32::try_from(body.len())
                    .map_err(|_| UpstreamError::Build("responses_ws_message_too_large".into()))?,
            )
            .map_err(|_| UpstreamError::Build("responses_ws_queue_full".into()))?;
        let (events, receiver) = mpsc::channel(32);
        let (finish, finished) = oneshot::channel();
        let (admission, admitted) = oneshot::channel();
        self.commands
            .send(Turn {
                lane,
                body,
                response_id: None,
                events,
                finish,
                admission: Some(admission),
                _slot: slot,
                bytes: Some(bytes),
            })
            .await
            .map_err(|_| UpstreamError::Stream("responses_ws_closed".into()))?;
        admitted
            .await
            .map_err(|_| UpstreamError::Stream("responses_ws_closed".into()))?
            .map_err(|code| UpstreamError::Build(code.into()))?;
        Ok(StreamHandle {
            upstream_request_id: self.request_id.clone(),
            events: receive(receiver, finished),
        })
    }

    pub fn close(&self) {
        self.close.send_replace(true);
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        *self.close.borrow() || self.commands.is_closed()
    }
}

struct Receiver {
    events: mpsc::Receiver<BufferedEvents>,
    finish: oneshot::Receiver<Result<(), Failure>>,
    buffered: Option<BufferedEvents>,
    ended: bool,
}

fn receive(
    events: mpsc::Receiver<BufferedEvents>,
    finish: oneshot::Receiver<Result<(), Failure>>,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, UpstreamError>> + Send>> {
    Box::pin(stream::unfold(
        Receiver {
            events,
            finish,
            buffered: None,
            ended: false,
        },
        |mut rx| async move {
            if rx.ended {
                return None;
            }
            loop {
                if let Some(event) = rx.buffered.as_mut().and_then(|b| b.events.pop_front()) {
                    if rx.buffered.as_ref().is_some_and(|b| b.events.is_empty()) {
                        rx.buffered = None;
                    }
                    return Some((Ok(event), rx));
                }
                rx.buffered = None;
                if let Some(events) = rx.events.recv().await {
                    rx.buffered = Some(events);
                } else {
                    rx.ended = true;
                    return match (&mut rx.finish).await {
                        Ok(Ok(())) => None,
                        Ok(Err(failure)) => Some((Err(failure.error()), rx)),
                        Err(_) => Some((Err(Failure::Stream("responses_ws_closed").error()), rx)),
                    };
                }
            }
        },
    ))
}
