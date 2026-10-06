//! Keep admission/resource ownership on a streaming response, through EOF/drop.
use crate::UpstreamError;
use futures::Stream;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

pub trait ResponseGuard: Send + Unpin + 'static {
    fn poll_expired(&mut self, cx: &mut Context<'_>) -> Poll<()>;
}
pub trait ResponseLifetime: Sized {
    #[must_use]
    fn with_guard(self, guard: impl ResponseGuard) -> Self;
}
type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, UpstreamError>> + Send>>;
struct GuardedStream<T, G> {
    stream: Option<EventStream<T>>,
    guard: Option<G>,
}
impl<T, G: ResponseGuard> Stream for GuardedStream<T, G> {
    type Item = Result<T, UpstreamError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(guard) = this.guard.as_mut() else {
            return Poll::Ready(None);
        };
        if guard.poll_expired(cx).is_ready() {
            this.stream.take();
            this.guard.take();
            return Poll::Ready(Some(Err(UpstreamError::Build(
                "channel_control:concurrency_lease_lost".into(),
            ))));
        }
        let next = match this.stream.as_mut() {
            Some(stream) => stream.as_mut().poll_next(cx),
            None => Poll::Ready(None),
        };
        if matches!(next, Poll::Ready(None | Some(Err(_)))) {
            this.stream.take();
            this.guard.take();
        }
        next
    }
}
pub fn guard_stream<T: 'static>(
    stream: Pin<Box<dyn Stream<Item = Result<T, UpstreamError>> + Send>>,
    guard: impl ResponseGuard,
) -> Pin<Box<dyn Stream<Item = Result<T, UpstreamError>> + Send>> {
    Box::pin(GuardedStream {
        stream: Some(stream),
        guard: Some(guard),
    })
}
macro_rules! stream_response {
    ($response:ty) => {
        impl ResponseLifetime for $response {
            fn with_guard(self, guard: impl ResponseGuard) -> Self {
                match self {
                    Self::Stream(mut stream) => {
                        stream.events = guard_stream(stream.events, guard);
                        Self::Stream(stream)
                    }
                    value => {
                        drop(guard);
                        value
                    }
                }
            }
        }
    };
}
stream_response!(crate::ChatResponse);
stream_response!(crate::anthropic::MessagesResponse);
stream_response!(crate::gemini::GeminiResponse);
stream_response!(crate::image_stream::ImageResponse);
impl ResponseLifetime for crate::custom_pass::PassResponse {
    fn with_guard(self, guard: impl ResponseGuard) -> Self {
        match self {
            Self::Ok {
                status,
                content_type,
                stream,
            } => Self::Ok {
                status,
                content_type,
                stream: guard_stream(stream, guard),
            },
            value @ Self::ErrStatus { .. } => {
                drop(guard);
                value
            }
        }
    }
}
impl ResponseLifetime for crate::openai::EmbeddingsResponse {
    fn with_guard(self, guard: impl ResponseGuard) -> Self {
        drop(guard);
        self
    }
}
impl ResponseLifetime for (u16, String, bytes::Bytes) {
    fn with_guard(self, guard: impl ResponseGuard) -> Self {
        drop(guard);
        self
    }
}

impl ResponseLifetime for crate::inference::Response {
    fn with_guard(self, guard: impl ResponseGuard) -> Self {
        match self {
            Self::OpenAi(response) => Self::OpenAi(response.with_guard(guard)),
            Self::Messages(response) => Self::Messages(response.with_guard(guard)),
            Self::Gemini(response) => Self::Gemini(response.with_guard(guard)),
        }
    }
}
