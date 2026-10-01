//! Images SSE: preview immediately, durable settlement before completed frames.
//! Client disconnect only drops delivery; a tracked, bounded pump still accounts for generation.
use super::{AppError, AppState, Prepared, commit_and_record, refund, request, scale_quote, usage};
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
};
use futures::StreamExt;
use okapi_api::codes;
use okapi_domain::TokenUsage;
use okapi_providers::{
    UpstreamError,
    image_stream::{ImageEvent, ImageResponse},
};
use serde::Deserialize;
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

mod collect;
mod metering;
use collect::Collected;
use metering::Mode;

pub(super) struct Context {
    pub state: AppState,
    pub key: okapi_store::AuthedKey,
    pub headers: HeaderMap,
    pub input: request::Input,
    pub request_id: Uuid,
    pub started: Instant,
    pub endpoint: String,
    pub reserved_pool: okapi_ledger::Pool,
    pub source_window: Option<String>,
    pub prepared: Prepared,
}

pub(super) struct Receipt {
    pub usage_mode: &'static str,
    pub usage_complete: bool,
    pub incomplete: bool,
    pub ttft_ms: Option<i32>,
}

type Sender = Option<mpsc::Sender<Result<Event, Infallible>>>;

pub(super) async fn start(
    ctx: Context,
    candidates: Vec<okapi_store::ChannelCandidate>,
) -> Result<Response, AppError> {
    let (tx, rx) = mpsc::channel(4);
    let (ready, opened) = oneshot::channel();
    let pending = ctx.state.settlements.clone();
    // Track the entire dispatch: cancellation while awaiting upstream headers must not orphan a bill.
    pending.spawn(async move {
        let mut failure = super::super::failure::Guard::new(&ctx.state,&ctx.key,ctx.request_id,&ctx.prepared.canonical,&ctx.input.model,&ctx.endpoint,ctx.started,ctx.reserved_pool,ctx.source_window.as_deref());
        let mut sender = Some(tx);
        let opened = tokio::time::timeout_at(deadline(&ctx), open(&ctx, candidates)).await
            .unwrap_or_else(|_| Err(AppError::new(StatusCode::GATEWAY_TIMEOUT, codes::UPSTREAM_TIMEOUT)));
        match opened {
            Ok((response, candidate, failovers, mode)) => {
                failure.channel(&candidate);
                let _ = ready.send(Ok(()));
                let mut collected = Collected::read(&ctx, response, mode, &mut sender).await;
                if let Err(error) = settle(&ctx, &candidate, failovers, &collected).await {
                    tracing::error!(request_id=%ctx.request_id, error=?error, "image stream settlement failed");
                    // A settlement failure may follow a committed PG transaction. Never refund blindly.
                    failure.error(&error);
                    collected.frames.clear();
                    collected.failed = true;
                } else if !collected.frames.is_empty() {
                    failure.disarm();
                }
                for frame in collected.frames {
                    deliver(&mut sender, frame).await;
                }
                if collected.failed {
                    deliver(&mut sender, error_event(ctx.request_id)).await;
                } else if collected.done {
                    deliver(&mut sender, Event::default().data("[DONE]")).await;
                }
            }
            Err(error) => {
                failure.error(&error);
                if let Err(refund_error) = refund(&ctx.state, &ctx.key, ctx.request_id, None).await {
                    tracing::error!(request_id=%ctx.request_id, error=?refund_error, "image stream refund failed");
                }
                let _ = ready.send(Err(error));
            }
        }
    });
    opened.await.map_err(|_| AppError::internal())??;
    let events = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    });
    Ok(Sse::new(events).into_response())
}

async fn open(
    ctx: &Context,
    candidates: Vec<okapi_store::ChannelCandidate>,
) -> Result<(ImageResponse, okapi_store::ChannelCandidate, i16, Mode), AppError> {
    for (index, candidate) in candidates.into_iter().take(super::MAX_ATTEMPTS).enumerate() {
        let mode = Mode::load(&ctx.state, candidate.channel_id).await?;
        let model = candidate.upstream_model(&ctx.prepared.canonical);
        match ctx
            .input
            .forward_stream(&ctx.state, &candidate, model, &ctx.endpoint)
            .await
        {
            Ok(response) => {
                return Ok((
                    response,
                    candidate,
                    i16::try_from(index).unwrap_or(i16::MAX),
                    mode,
                ));
            }
            Err(
                error @ UpstreamError::Status {
                    status: 401 | 402 | 403 | 429,
                    ..
                },
            ) => {
                let _ = okapi_store::channels::mark_key_failure(
                    &ctx.state.pg,
                    candidate.channel_key_id,
                    error.error_code(),
                    super::super::chat::failure_kind_of(&error),
                )
                .await;
            }
            Err(error) => return Err(AppError::new(StatusCode::BAD_GATEWAY, error.error_code())),
        }
    }
    Err(AppError::new(
        StatusCode::BAD_GATEWAY,
        codes::UPSTREAM_ERROR,
    ))
}

async fn settle(
    ctx: &Context,
    candidate: &okapi_store::ChannelCandidate,
    failovers: i16,
    result: &Collected,
) -> Result<(), AppError> {
    if result.frames.is_empty() {
        return refund(&ctx.state, &ctx.key, ctx.request_id, None).await;
    }
    let actual = u32::try_from(result.frames.len()).map_err(|_| AppError::internal())?;
    let p = &ctx.prepared;
    let quote = if p.unit_quote.snapshot.mode == "per_call" {
        scale_quote(&p.unit_quote, actual)
    } else {
        result.usage.ok_or_else(usage::invalid).and_then(|usage| {
            okapi_pricing::calculate(&p.book, &p.calc, usage).map_err(AppError::from)
        })
    };
    let mut quote = match quote {
        Ok(quote) => quote,
        Err(error) => {
            refund(&ctx.state, &ctx.key, ctx.request_id, None).await?;
            return Err(error);
        }
    };
    quote.snapshot.media_units = Some(actual);
    let upstream = okapi_providers::openai::EmbeddingsResponse {
        status: result.status,
        upstream_request_id: result.upstream_request_id.clone(),
        body: bytes::Bytes::new(),
        usage: None,
    };
    commit_and_record(
        &ctx.state,
        &ctx.key,
        &p.canonical,
        &ctx.input.model,
        &quote,
        result.usage,
        p.pricing_epoch,
        ctx.request_id,
        ctx.started,
        candidate,
        failovers,
        &ctx.headers,
        &ctx.endpoint,
        &upstream,
        None,
        ctx.reserved_pool,
        ctx.source_window.as_deref(),
        Some(Receipt {
            usage_mode: result.mode,
            usage_complete: result.usage_complete,
            incomplete: result.failed,
            ttft_ms: result.ttft_ms,
        }),
    )
    .await
}

async fn deliver(sender: &mut Sender, event: Event) {
    if let Some(tx) = sender {
        // Slow clients cannot keep the reservation alive indefinitely or block upstream accounting.
        if !matches!(
            tokio::time::timeout(Duration::from_secs(10), tx.send(Ok(event))).await,
            Ok(Ok(()))
        ) {
            *sender = None;
        }
    }
}

fn deadline(ctx: &Context) -> tokio::time::Instant {
    tokio::time::Instant::from_std(
        ctx.started + okapi_providers::image_stream::IMAGE_STREAM_TIMEOUT,
    )
}

fn error_event(id: Uuid) -> Event {
    Event::default().event("error").data(
        serde_json::json!({
            "error":{"code":codes::UPSTREAM_ERROR,"params":{},"request_id":id}
        })
        .to_string(),
    )
}

#[derive(Deserialize)]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    b64_json: Option<String>,
    url: Option<String>,
    partial_image_index: Option<u32>,
}

fn parse(event: &ImageEvent, prefix: &str) -> Result<(Frame, bool), AppError> {
    let frame: Frame = serde_json::from_str(&event.data).map_err(|_| usage::invalid())?;
    let completed = frame.kind == format!("{prefix}.completed");
    if (!event.event.is_empty() && event.event != "message" && event.event != frame.kind)
        || (!completed && frame.kind != format!("{prefix}.partial_image"))
        || (!completed && frame.partial_image_index.is_none_or(|index| index >= 3))
        || ![&frame.b64_json, &frame.url]
            .iter()
            .any(|value| value.as_ref().is_some_and(|s| !s.trim().is_empty()))
    {
        return Err(usage::invalid());
    }
    Ok((frame, completed))
}
