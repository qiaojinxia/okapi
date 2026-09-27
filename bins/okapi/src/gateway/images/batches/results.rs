use super::{
    AppError, AppState, Batch, binding::Remote, map_store, request::slot_from_key, store, upstream,
};
use base64::Engine as _;
use okapi_domain::TokenUsage;
use okapi_providers::batch::{Job, JobState, Output, jsonl};
use serde_json::{Value, json};
use std::collections::HashSet;
mod gcs;

pub(super) fn reference(job: &Job) -> Value {
    match &job.output {
        Some(Output::File(name)) => json!({"kind":"gemini_file","name":name}),
        Some(Output::Inline(_)) => json!({"kind":"inline"}),
        Some(Output::GcsPrefix(uri)) => json!({"kind":"gcs","uri":uri}),
        None => json!({}),
    }
}
pub(super) async fn collect(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
    remote: &Remote,
    job: Job,
) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    // Storage admission already accounts for raw images, input and overhead.
    // Twice that allowance covers base64 and echoed input, across all result files.
    let budget = usize::try_from(row.storage_budget)
        .ok()
        .and_then(|n| n.checked_mul(2))
        .filter(|n| *n > 0 && *n <= jsonl::MAX_RESULT_BYTES)
        .ok_or_else(|| invalid("batch_result_budget"))?;
    match (remote, job.output) {
        (Remote::Gemini(client), Some(Output::File(name))) => {
            let mut reader = client
                .download(&name, limits(budget)?)
                .await
                .map_err(|e| upstream(&e))?;
            read(state, row, lease, &mut reader, &mut seen).await?;
        }
        (Remote::Gemini(_), Some(Output::Inline(items))) => {
            if items.len() > 200 {
                return Err(invalid("batch_result_count"));
            }
            for item in items {
                stage(state, row, lease, item, &mut seen).await?;
            }
        }
        (Remote::Vertex(_, files), Some(Output::GcsPrefix(directory))) => {
            gcs::collect(state, row, lease, files, &directory, &mut seen, budget).await?;
        }
        (_, None)
            if !matches!(
                job.state,
                okapi_providers::batch::JobState::Succeeded
                    | okapi_providers::batch::JobState::PartiallySucceeded
            ) => {}
        _ => return Err(invalid("batch_result_source")),
    }
    if matches!(
        job.state,
        JobState::Succeeded | JobState::PartiallySucceeded
    ) && seen.len()
        != usize::try_from(row.output_count).map_err(|_| invalid("batch_result_count"))?
    {
        return Err(invalid("batch_results_incomplete"));
    }
    for slot in 0..u32::try_from(row.output_count).map_err(|_| invalid("batch_result_count"))? {
        if !seen.contains(&slot) {
            store::stage(
                &state.pg,
                lease,
                slot,
                store::Output::Failed {
                    error_code: "batch_result_missing",
                    usage: &serde_json::to_value(TokenUsage::default())
                        .map_err(|_| AppError::internal())?,
                },
            )
            .await
            .map_err(map_store)?;
        }
    }
    store::seal_results(&state.pg, lease)
        .await
        .map_err(map_store)?;
    Ok(())
}
fn limits(remaining: usize) -> Result<jsonl::Limits, AppError> {
    if remaining == 0 {
        return Err(invalid("batch_result_budget"));
    }
    Ok(jsonl::Limits {
        line_bytes: remaining.min(64 * 1024 * 1024),
        total_bytes: remaining,
        rows: 200,
    })
}
async fn read(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
    reader: &mut jsonl::Reader,
    seen: &mut HashSet<u32>,
) -> Result<(), AppError> {
    while let Some(value) = reader.next().await.map_err(|e| upstream(&e))? {
        stage(state, row, lease, value, seen).await?;
    }
    Ok(())
}
fn invalid(param: &'static str) -> AppError {
    AppError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        okapi_api::codes::UPSTREAM_ERROR,
    )
    .with_param(param)
}
async fn stage(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
    value: Value,
    seen: &mut HashSet<u32>,
) -> Result<(), AppError> {
    let key = value
        .get("key")
        .or_else(|| value.pointer("/metadata/key"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("batch_result_key"))?;
    let slot = slot_from_key(row.id, key, row.output_count)?;
    if !seen.insert(slot) {
        return Err(invalid("batch_duplicate_result"));
    }
    let error = value.get("error").filter(|v| !v.is_null());
    let status = value.get("status").filter(|v| {
        !v.is_null() && v.as_str() != Some("") && v.get("code").and_then(Value::as_i64) != Some(0)
    });
    let response = value.get("response").filter(|v| !v.is_null());
    if response.is_some_and(|v| !v.is_object()) {
        return Err(invalid("batch_result_response"));
    }
    // Vertex returns response:{} together with failed status text. Only real
    // response content conflicts with an error; an empty placeholder does not.
    if (error.is_some() || status.is_some())
        && response.is_some_and(|v| !v.as_object().is_some_and(serde_json::Map::is_empty))
    {
        return Err(invalid("batch_result_conflict"));
    }
    let usage = usage(response.and_then(|v| v.get("usageMetadata")))?;
    let usage = serde_json::to_value(usage).map_err(|_| AppError::internal())?;
    let image = if error.is_some() || status.is_some() {
        None
    } else {
        let response = response.ok_or_else(|| invalid("batch_result_response"))?;
        image(response)?
    };
    let output = match &image {
        Some((bytes, mime)) => store::Output::Success {
            content: bytes,
            content_type: mime,
            usage: &usage,
        },
        None => store::Output::Failed {
            error_code: if error.is_some() || status.is_some() {
                "batch_item_failed"
            } else {
                "batch_no_image"
            },
            usage: &usage,
        },
    };
    store::stage(&state.pg, lease, slot, output)
        .await
        .map_err(map_store)?;
    Ok(())
}
fn image(response: &Value) -> Result<Option<(Vec<u8>, String)>, AppError> {
    let candidates = match response.get("candidates") {
        None => return Ok(None),
        Some(v) => v.as_array().ok_or_else(|| invalid("batch_candidates"))?,
    };
    let mut found = None;
    for candidate in candidates {
        if let Some(parts) = candidate.pointer("/content/parts") {
            for part in parts
                .as_array()
                .ok_or_else(|| invalid("batch_image_parts"))?
            {
                if let Some(inline) = part.get("inlineData") {
                    let mime = inline
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("batch_image_mime"))?;
                    let data = inline
                        .get("data")
                        .and_then(Value::as_str)
                        .filter(|s| s.len() <= 24 * 1024 * 1024)
                        .ok_or_else(|| invalid("batch_image_size"))?;
                    let bytes = base64::prelude::BASE64_STANDARD
                        .decode(data)
                        .map_err(|_| invalid("batch_image_base64"))?;
                    if found.is_some()
                        || bytes.len() > store::MAX_IMAGE_BYTES
                        || !matches!(mime, "image/png" | "image/jpeg" | "image/webp")
                        || okapi_providers::image_store::fetch::content_type(&bytes) != Some(mime)
                    {
                        return Err(invalid("batch_image_bytes"));
                    }
                    found = Some((bytes, mime.to_owned()));
                }
            }
        }
    }
    Ok(found)
}
fn number(value: &Value, key: &str) -> Result<u32, AppError> {
    match value.get(key) {
        None => Ok(0),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| invalid("batch_usage")),
    }
}
fn usage(value: Option<&Value>) -> Result<TokenUsage, AppError> {
    let Some(value) = value else {
        return Ok(TokenUsage::default());
    };
    if !value.is_object() {
        return Err(invalid("batch_usage"));
    }
    let reasoning = number(value, "thoughtsTokenCount")?;
    let completion = number(value, "candidatesTokenCount")?
        .checked_add(reasoning)
        .ok_or_else(|| invalid("batch_usage"))?;
    let usage = TokenUsage {
        prompt_tokens: number(value, "promptTokenCount")?,
        cached_tokens: number(value, "cachedContentTokenCount")?,
        completion_tokens: completion,
        reasoning_tokens: reasoning,
        ..TokenUsage::default()
    };
    usage.validate().map_err(|_| invalid("batch_usage"))?;
    if value.get("totalTokenCount").is_some()
        && u64::from(number(value, "totalTokenCount")?) != usage.total_raw()
    {
        return Err(invalid("batch_usage_total"));
    }
    Ok(usage)
}
pub(super) fn total_usage(values: Vec<Value>) -> Result<TokenUsage, AppError> {
    let mut total = TokenUsage::default();
    for value in values {
        let u: TokenUsage = if value == json!({}) {
            TokenUsage::default()
        } else {
            serde_json::from_value(value).map_err(|_| invalid("batch_usage"))?
        };
        u.validate().map_err(|_| invalid("batch_usage"))?;
        let add = |a: u32, b: u32| a.checked_add(b).ok_or_else(|| invalid("batch_usage_total"));
        total.prompt_tokens = add(total.prompt_tokens, u.prompt_tokens)?;
        total.cached_tokens = add(total.cached_tokens, u.cached_tokens)?;
        total.completion_tokens = add(total.completion_tokens, u.completion_tokens)?;
        total.reasoning_tokens = add(total.reasoning_tokens, u.reasoning_tokens)?;
        total.cache_write_tokens = add(total.cache_write_tokens, u.cache_write_tokens)?;
        total.image_prompt_tokens = add(total.image_prompt_tokens, u.image_prompt_tokens)?;
        total.audio_prompt_tokens = add(total.audio_prompt_tokens, u.audio_prompt_tokens)?;
        total.audio_completion_tokens =
            add(total.audio_completion_tokens, u.audio_completion_tokens)?;
    }
    total.validate().map_err(|_| invalid("batch_usage_total"))?;
    Ok(total)
}
