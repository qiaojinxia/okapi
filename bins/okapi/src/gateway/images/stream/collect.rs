use super::{
    AppError, Context, Event, ImageResponse, Mode, Sender, StreamExt, TokenUsage, deliver, parse,
    request, usage,
};

pub(super) struct Collected {
    pub frames: Vec<Event>,
    pub usage: Option<TokenUsage>,
    pub mode: &'static str,
    pub usage_complete: bool,
    pub status: u16,
    pub upstream_request_id: Option<String>,
    pub failed: bool,
    pub done: bool,
    pub ttft_ms: Option<i32>,
}

impl Collected {
    pub async fn read(
        ctx: &Context,
        response: ImageResponse,
        mode: Mode,
        sender: &mut Sender,
    ) -> Self {
        let mut result = Self {
            frames: Vec::new(),
            usage: None,
            mode: mode.label(),
            usage_complete: true,
            status: 200,
            upstream_request_id: None,
            failed: false,
            done: false,
            ttft_ms: None,
        };
        let prefix = if ctx.endpoint.ends_with("edits") {
            "image_edit"
        } else {
            "image_generation"
        };
        let required = ctx.prepared.unit_quote.snapshot.mode != "per_call";
        match response {
            ImageResponse::Json(response) => {
                result.status = response.status;
                result
                    .upstream_request_id
                    .clone_from(&response.upstream_request_id);
                result.failed = result
                    .json(&response.body, ctx.input.units, prefix, required)
                    .is_err();
                result.ttft_ms =
                    Some(i32::try_from(ctx.started.elapsed().as_millis()).unwrap_or(i32::MAX));
            }
            ImageResponse::Stream(mut stream) => {
                result.status = stream.status;
                result.upstream_request_id = stream.upstream_request_id;
                // The provider's request timeout also bounds waiting for an unterminated SSE frame.
                loop {
                    let event =
                        match tokio::time::timeout_at(super::deadline(ctx), stream.events.next())
                            .await
                        {
                            Ok(Some(event)) => event,
                            Ok(None) => break,
                            Err(_) => {
                                result.failed = true;
                                break;
                            }
                        };
                    let Ok(event) = event else {
                        result.failed = true;
                        break;
                    };
                    if event.data == "[DONE]" {
                        result.done = true;
                        break;
                    }
                    let Ok((frame, completed)) = parse(&event, prefix) else {
                        result.failed = true;
                        break;
                    };
                    result.ttft_ms.get_or_insert_with(|| {
                        i32::try_from(ctx.started.elapsed().as_millis()).unwrap_or(i32::MAX)
                    });
                    if completed {
                        if result.frames.len() >= ctx.input.units as usize {
                            result.failed = true;
                            break;
                        }
                        let usage =
                            usage::parse(event.data.as_bytes(), required).and_then(|next| {
                                mode.combine(result.usage, next)
                                    .map(|total| (total, next.is_some()))
                            });
                        let Ok(usage) = usage else {
                            result.failed = true;
                            break;
                        };
                        result.usage = usage.0;
                        result.usage_complete = match mode {
                            Mode::Cumulative => usage.1,
                            Mode::PerImage => result.usage_complete && usage.1,
                        };
                        result
                            .frames
                            .push(Event::default().event(frame.kind).data(event.data));
                    } else {
                        deliver(sender, Event::default().event(frame.kind).data(event.data)).await;
                    }
                }
            }
        }
        result.failed |= result.frames.is_empty();
        result
    }

    fn json(
        &mut self,
        bytes: &bytes::Bytes,
        requested: u32,
        prefix: &str,
        required: bool,
    ) -> Result<(), AppError> {
        request::returned_images(bytes, requested)?;
        self.usage = usage::parse(bytes, required)?;
        self.usage_complete = self.usage.is_some();
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| usage::invalid())?;
        let data = value["data"].as_array().ok_or_else(usage::invalid)?;
        let kind = format!("{prefix}.completed");
        for (index, item) in data.iter().enumerate() {
            let mut frame = item.clone();
            frame["type"] = kind.clone().into();
            for key in ["created", "size", "quality", "background", "output_format"] {
                if let Some(value) = value.get(key) {
                    frame[if key == "created" { "created_at" } else { key }] = value.clone();
                }
            }
            if index + 1 == data.len()
                && let Some(usage) = value.get("usage")
            {
                frame["usage"] = usage.clone();
            }
            self.frames
                .push(Event::default().event(&kind).data(frame.to_string()));
        }
        self.mode = "response";
        self.done = true;
        Ok(())
    }
}
