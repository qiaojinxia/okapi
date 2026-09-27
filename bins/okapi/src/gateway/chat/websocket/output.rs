use super::{AppError, Lane, MAX_BUFFER, Message, Semaphore};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, mpsc, watch};
use tokio::time::Instant;
use uuid::Uuid;

pub(super) struct Event {
    pub message: Message,
    _bytes: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(super) struct Output {
    events: mpsc::Sender<Event>,
    bytes: Arc<Semaphore>,
    pub closed: watch::Sender<bool>,
    pub activity: watch::Sender<Instant>,
}

impl Output {
    pub fn new() -> (Self, mpsc::Receiver<Event>) {
        let (events, rx) = mpsc::channel(64);
        let (closed, _) = watch::channel(false);
        let (activity, _) = watch::channel(Instant::now());
        (
            Self {
                events,
                bytes: Arc::new(Semaphore::new(MAX_BUFFER)),
                closed,
                activity,
            },
            rx,
        )
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }
    pub fn close(&self) {
        self.closed.send_replace(true);
    }
    pub fn touch(&self) {
        self.activity.send_replace(Instant::now());
    }

    pub fn push(&self, message: Message) {
        if self.is_closed() {
            return;
        }
        let length = match &message {
            Message::Text(value) => value.len(),
            Message::Binary(value) | Message::Ping(value) | Message::Pong(value) => value.len(),
            Message::Close(_) => 0,
        };
        let Ok(length) = u32::try_from(length) else {
            self.close();
            return;
        };
        let Ok(bytes) = self.bytes.clone().try_acquire_many_owned(length) else {
            self.close();
            return;
        };
        if self
            .events
            .try_send(Event {
                message,
                _bytes: bytes,
            })
            .is_err()
        {
            // Stop downstream delivery without blocking the billing consumer.
            self.close();
        } else {
            self.touch();
        }
    }

    pub fn data(&self, raw: &str, request: Uuid) {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
            self.close();
            return;
        };
        if !value.is_object() {
            self.close();
            return;
        }
        value["okapi_request_id"] = request.to_string().into();
        self.push(Message::Text(value.to_string().into()));
    }

    pub fn error(&self, error: &AppError, request: Uuid, lane: Lane) {
        let mut value = serde_json::json!({ "type":"error", "status":error.status.as_u16(),
            "error":{"type":error.code,"code":error.code,"message":error.code,"param":error.param},
            "okapi_request_id":request });
        if let Some(lane) = lane {
            value["stream_id"] = lane.into();
        }
        self.push(Message::Text(value.to_string().into()));
    }
}
