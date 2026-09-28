//! Validate requested cardinality before estimating charges or forwarding.
use super::AppError;
use super::AppState;
use axum::extract::{FromRequest, Request};
use axum::http::header;
use base64::Engine as _;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub(super) struct Input {
    pub model: String,
    pub units: u32,
    pub stream: bool,
    partial_images: u32,
    body: Payload,
}

enum Payload {
    Json(Bytes),
    Multipart(Vec<Part>),
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u8,
    payload: StoredPayload,
}

#[derive(Serialize, Deserialize)]
enum StoredPayload {
    Json(String),
    Multipart(Vec<(String, Option<String>, Option<String>, String)>),
}

impl Input {
    /// Admission estimate only: a byte bound for text and a per-reference image allowance.
    /// Actual billing always uses upstream usage; URLs/base64 are never counted as text tokens.
    pub fn estimate(&self, output_per_image: u32) -> Result<okapi_domain::TokenUsage, AppError> {
        let (text_bytes, images) = match &self.body {
            Payload::Json(bytes) => {
                let probe: Probe =
                    serde_json::from_slice(bytes).map_err(|_| AppError::bad_request())?;
                (
                    probe.prompt.len(),
                    probe
                        .images
                        .as_ref()
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len)
                        .saturating_add(usize::from(probe.mask.is_some())),
                )
            }
            Payload::Multipart(parts) => (
                parts
                    .iter()
                    .filter(|(name, _, _, _)| name == "prompt")
                    .map(|(_, _, _, bytes)| bytes.len())
                    .sum(),
                parts
                    .iter()
                    .filter(|(name, _, _, _)| matches!(name.as_str(), "image" | "image[]" | "mask"))
                    .count(),
            ),
        };
        let invalid = || AppError::bad_request().with_param("image_token_estimate");
        let image_tokens = u32::try_from(images)
            .ok()
            .and_then(|v| v.checked_mul(8192))
            .ok_or_else(invalid)?;
        let prompt_tokens = u32::try_from(text_bytes)
            .ok()
            .and_then(|v| v.checked_add(image_tokens))
            .ok_or_else(invalid)?;
        let completion_tokens = output_per_image
            .checked_add(self.partial_images * 100)
            .and_then(|value| value.checked_mul(self.units))
            .ok_or_else(invalid)?;
        Ok(okapi_domain::TokenUsage {
            prompt_tokens,
            image_prompt_tokens: image_tokens,
            image_completion_tokens: completion_tokens,
            completion_tokens,
            ..okapi_domain::TokenUsage::default()
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, AppError> {
        let encode = |bytes: &Bytes| base64::prelude::BASE64_STANDARD.encode(bytes);
        let payload = match &self.body {
            Payload::Json(bytes) => StoredPayload::Json(encode(bytes)),
            Payload::Multipart(parts) => StoredPayload::Multipart(
                parts
                    .iter()
                    .map(|(name, file, mime, bytes)| {
                        (name.clone(), file.clone(), mime.clone(), encode(bytes))
                    })
                    .collect(),
            ),
        };
        serde_json::to_vec(&Stored {
            version: 1,
            payload,
        })
        .map_err(|_| AppError::internal())
    }

    pub fn decode(bytes: &[u8], edit: bool) -> Result<Self, AppError> {
        let stored: Stored = serde_json::from_slice(bytes).map_err(|_| AppError::internal())?;
        if stored.version != 1 {
            return Err(AppError::internal());
        }
        let decode = |text: &str| {
            base64::prelude::BASE64_STANDARD
                .decode(text)
                .map(Bytes::from)
                .map_err(|_| AppError::internal())
        };
        match stored.payload {
            StoredPayload::Json(text) => {
                let bytes = decode(&text)?;
                if bytes.len() > 32 * 1024 * 1024 {
                    return Err(AppError::internal());
                }
                let (model, units, stream, partial_images, body) = json(&bytes, edit)?;
                Ok(Self {
                    model,
                    units,
                    stream,
                    partial_images,
                    body: Payload::Json(body),
                })
            }
            StoredPayload::Multipart(parts) if edit => {
                let parts: Vec<_> = parts
                    .into_iter()
                    .map(|(name, file, mime, text)| Ok((name, file, mime, decode(&text)?)))
                    .collect::<Result<_, AppError>>()?;
                if parts
                    .iter()
                    .map(|(_, _, _, bytes)| bytes.len())
                    .sum::<usize>()
                    > 32 * 1024 * 1024
                {
                    return Err(AppError::internal());
                }
                let (model, units, stream, partial_images, parts) = multipart(parts)?;
                Ok(Self {
                    model,
                    units,
                    stream,
                    partial_images,
                    body: Payload::Multipart(parts),
                })
            }
            StoredPayload::Multipart(_) => Err(AppError::internal()),
        }
    }

    pub async fn forward(
        &self,
        state: &AppState,
        candidate: &okapi_store::ChannelCandidate,
        model: &str,
        endpoint: &str,
    ) -> Result<okapi_providers::openai::EmbeddingsResponse, okapi_providers::UpstreamError> {
        let path = endpoint.strip_prefix("/v1").unwrap_or(endpoint);
        match &self.body {
            Payload::Json(body) => {
                let body = okapi_providers::rewrite_model(body, &self.model, model)?;
                state.openai_json(candidate, model, path, body).await
            }
            Payload::Multipart(parts) => {
                let mut parts = parts.clone();
                for (name, _, _, data) in &mut parts {
                    if name == "model" {
                        *data = Bytes::copy_from_slice(model.as_bytes());
                    }
                }
                state
                    .openai_audio_multipart(candidate, model, path, parts)
                    .await
            }
        }
    }

    pub fn require_nonstream(&self) -> Result<(), AppError> {
        if self.stream {
            return Err(AppError::bad_request().with_param("image_task_streaming_unsupported"));
        }
        Ok(())
    }

    pub async fn forward_stream(
        &self,
        state: &AppState,
        candidate: &okapi_store::ChannelCandidate,
        model: &str,
        endpoint: &str,
    ) -> Result<okapi_providers::image_stream::ImageResponse, okapi_providers::UpstreamError> {
        use okapi_providers::image_stream::ImageBody;
        let body = match &self.body {
            Payload::Json(body) => {
                ImageBody::Json(okapi_providers::rewrite_model(body, &self.model, model)?)
            }
            Payload::Multipart(parts) => {
                let mut parts = parts.clone();
                for (name, _, _, data) in &mut parts {
                    if name == "model" {
                        *data = Bytes::copy_from_slice(model.as_bytes());
                    }
                }
                ImageBody::Multipart(parts)
            }
        };
        state
            .openai_image_stream(
                candidate,
                model,
                endpoint.strip_prefix("/v1").unwrap_or(endpoint),
                body,
            )
            .await
    }
}

pub(super) async fn read(req: Request, state: &AppState, edit: bool) -> Result<Input, AppError> {
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("multipart/form-data"));
    if edit && is_multipart {
        let mut form = axum::extract::Multipart::from_request(req, state)
            .await
            .map_err(|error| {
                AppError::new(error.status(), okapi_api::codes::BAD_REQUEST).with_param("multipart")
            })?;
        let mut parts = Vec::new();
        while let Some(field) = form
            .next_field()
            .await
            .map_err(|error| multipart_error(&error))?
        {
            let name = field.name().unwrap_or_default().to_owned();
            let filename = field.file_name().map(str::to_owned);
            let content_type = field.content_type().map(str::to_owned);
            let bytes = field
                .bytes()
                .await
                .map_err(|error| multipart_error(&error))?;
            parts.push((name, filename, content_type, bytes));
        }
        let (model, units, stream, partial_images, parts) = multipart(parts)?;
        Ok(Input {
            model,
            units,
            stream,
            partial_images,
            body: Payload::Multipart(parts),
        })
    } else {
        let bytes = Bytes::from_request(req, state).await.map_err(|error| {
            AppError::new(error.status(), okapi_api::codes::BAD_REQUEST).with_param("body")
        })?;
        let (model, units, stream, partial_images, body) = json(&bytes, edit)?;
        Ok(Input {
            model,
            units,
            stream,
            partial_images,
            body: Payload::Json(body),
        })
    }
}

fn multipart_error(error: &axum::extract::multipart::MultipartError) -> AppError {
    AppError::new(error.status(), okapi_api::codes::BAD_REQUEST).with_param("multipart")
}

pub(super) type Part = (String, Option<String>, Option<String>, Bytes);

#[derive(Deserialize)]
struct Probe {
    model: String,
    prompt: String,
    n: Option<u32>,
    stream: Option<bool>,
    partial_images: Option<u32>,
    images: Option<Value>,
    mask: Option<Value>,
}

fn units(value: Option<u32>) -> Result<u32, AppError> {
    let value = value.unwrap_or(1);
    if !(1..=10).contains(&value) {
        return Err(AppError::bad_request().with_param("n"));
    }
    Ok(value)
}

fn text<'a>(value: &'a [u8], param: &str) -> Result<&'a str, AppError> {
    std::str::from_utf8(value)
        .map(str::trim)
        .map_err(|_| AppError::bad_request().with_param(param))
}

fn require_text(value: &str, param: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(AppError::bad_request().with_param(param));
    }
    Ok(())
}

fn partials(stream: bool, value: Option<u32>) -> Result<u32, AppError> {
    let value = value.unwrap_or(0);
    if value > 3 || (!stream && value != 0) {
        return Err(AppError::bad_request().with_param("partial_images"));
    }
    Ok(value)
}

fn reference(value: &Value) -> Result<(), AppError> {
    // Shared upstream files require a tenant ownership registry; never relay an arbitrary file ID.
    if value.get("file_id").is_some() {
        return Err(AppError::bad_request().with_param("image_file_id_requires_ownership"));
    }
    let url = value
        .get("image_url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::bad_request().with_param("image_url"))?;
    if url.starts_with("data:image/") {
        return Ok(());
    }
    if reqwest::Url::parse(url)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
    {
        return Ok(());
    }
    Err(AppError::bad_request().with_param("image_url"))
}

pub(super) fn json(body: &Bytes, edit: bool) -> Result<(String, u32, bool, u32, Bytes), AppError> {
    // Deserialize directly first: duplicate billing/identity fields must not use last-value-wins.
    let probe: Probe = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    require_text(&probe.model, "model")?;
    require_text(&probe.prompt, "prompt")?;
    let n = units(probe.n)?;
    let stream = probe.stream.unwrap_or(false);
    let partial_images = partials(stream, probe.partial_images)?;
    if edit {
        let images = probe
            .images
            .as_ref()
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| AppError::bad_request().with_param("images"))?;
        for image in images {
            reference(image)?;
        }
        if let Some(mask) = &probe.mask {
            reference(mask)?;
        }
    }
    let mut body: Value = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    body["n"] = n.into();
    Ok((
        probe.model,
        n,
        stream,
        partial_images,
        Bytes::from(body.to_string()),
    ))
}

pub(super) fn multipart(
    mut parts: Vec<Part>,
) -> Result<(String, u32, bool, u32, Vec<Part>), AppError> {
    let mut seen = HashSet::new();
    let mut model = None;
    let mut prompt = None;
    let mut count = None;
    let mut images = 0;
    let mut stream = false;
    let mut partial_images = None;
    for (name, _, _, data) in &mut parts {
        if matches!(name.as_str(), "image" | "image[]") {
            if data.is_empty() {
                return Err(AppError::bad_request().with_param("image"));
            }
            images += 1;
            continue;
        }
        if !seen.insert(name.clone()) {
            return Err(AppError::bad_request().with_param(name.clone()));
        }
        match name.as_str() {
            "model" => {
                model = Some(text(data, "model")?.to_owned());
            }
            "prompt" => {
                prompt = Some(text(data, "prompt")?.to_owned());
            }
            "n" => {
                let raw = text(data, "n")?;
                if raw.is_empty() || !raw.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(AppError::bad_request().with_param("n"));
                }
                let n = units(Some(
                    raw.parse()
                        .map_err(|_| AppError::bad_request().with_param("n"))?,
                ))?;
                count = Some(n);
                *data = Bytes::from(n.to_string());
            }
            "stream" => {
                stream = match text(data, "stream")? {
                    "false" => false,
                    "true" => true,
                    _ => return Err(AppError::bad_request().with_param("stream")),
                };
                *data = Bytes::from_static(if stream { b"true" } else { b"false" });
            }
            "partial_images" => {
                let value = text(data, "partial_images")?;
                if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(AppError::bad_request().with_param("partial_images"));
                }
                let count = value
                    .parse::<u32>()
                    .map_err(|_| AppError::bad_request().with_param("partial_images"))?;
                partial_images = Some(count);
                *data = Bytes::from(count.to_string());
            }
            _ => {}
        }
    }
    let model = model.ok_or_else(|| AppError::bad_request().with_param("model"))?;
    require_text(&model, "model")?;
    require_text(prompt.as_deref().unwrap_or_default(), "prompt")?;
    if images == 0 {
        return Err(AppError::bad_request().with_param("image"));
    }
    let n = units(count)?;
    if count.is_none() {
        parts.push(("n".into(), None, None, Bytes::from_static(b"1")));
    }
    Ok((model, n, stream, partials(stream, partial_images)?, parts))
}

pub(super) fn returned_images(body: &Bytes, requested: u32) -> Result<u32, AppError> {
    let invalid = || {
        AppError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            okapi_api::codes::UPSTREAM_ERROR,
        )
        .with_param("invalid_image_response")
    };
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let actual = u32::try_from(data.len()).map_err(|_| invalid())?;
    if actual == 0
        || actual > requested
        || data.iter().any(|item| {
            !["url", "b64_json"].iter().any(|key| {
                item.get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
            })
        })
    {
        return Err(invalid());
    }
    Ok(actual)
}
