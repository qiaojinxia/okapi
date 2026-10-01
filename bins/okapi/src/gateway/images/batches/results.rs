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
fn usage(value: Option<&Value>) -> Result<TokenUsage, AppError> {
    let Some(value) = value else {
        return Ok(TokenUsage::default());
    };
    okapi_providers::convert::openai_to_gemini::usage_from_gemini(Some(value))
        .ok_or_else(|| invalid("batch_usage"))?
        .to_token_usage()
        .map_err(|_| invalid("batch_usage"))
}
pub(super) fn total_usage(values: Vec<Value>) -> Result<TokenUsage, AppError> {
    let mut total: Option<TokenUsage> = None;
    for value in values {
        let u: TokenUsage = if value == json!({}) {
            TokenUsage::default()
        } else {
            serde_json::from_value(value).map_err(|_| invalid("batch_usage"))?
        };
        u.validate().map_err(|_| invalid("batch_usage"))?;
        if i32::try_from(u.prompt_tokens).is_err() || i32::try_from(u.completion_tokens).is_err() {
            return Err(invalid("batch_usage_total"));
        }
        total = Some(match total {
            None => u,
            Some(previous) => previous
                .checked_add(u)
                .map_err(|_| invalid("batch_usage_total"))?,
        });
    }
    Ok(total.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_totals_keep_image_output_cache_intersections_ttl_and_observation() {
        let raw = json!({"promptTokenCount":1000,"candidatesTokenCount":380,"thoughtsTokenCount":20,
            "cachedContentTokenCount":300,"totalTokenCount":1400,
            "promptTokensDetails":[{"modality":"TEXT","tokenCount":200},{"modality":"AUDIO","tokenCount":500},{"modality":"IMAGE","tokenCount":300}],
            "cacheTokensDetails":[{"modality":"TEXT","tokenCount":50},{"modality":"AUDIO","tokenCount":150},{"modality":"IMAGE","tokenCount":100}],
            "candidatesTokensDetails":[{"modality":"TEXT","tokenCount":80},{"modality":"AUDIO","tokenCount":100},{"modality":"IMAGE","tokenCount":200}]});
        let mut u = usage(Some(&raw)).unwrap();
        u.cache_write_reported = true;
        u.cache_write_tokens = 100;
        u.cache_write_5m_tokens = Some(40);
        u.cache_write_1h_tokens = Some(60);
        u.cache_write_modalities = Some(okapi_domain::CacheModalities {
            audio_tokens: 10,
            image_tokens: 20,
        });
        u.audio_prompt_tokens -= 10;
        u.image_prompt_tokens -= 20;
        u.reported_details.as_mut().unwrap().cache_write = okapi_domain::ModalitiesReported {
            audio: true,
            image: true,
        };
        let one = serde_json::to_value(u).unwrap();
        assert_eq!(total_usage(vec![one.clone()]).unwrap(), u);
        let total = total_usage(vec![one.clone(), one]).unwrap();
        assert_eq!(
            (
                total.prompt_tokens,
                total.completion_tokens,
                total.image_completion_tokens,
                total.audio_completion_tokens,
                total.reasoning_tokens
            ),
            (2000, 800, 400, 200, 40)
        );
        assert_eq!(
            (
                total.cached_tokens,
                total.cache_write_tokens,
                total.audio_prompt_tokens,
                total.image_prompt_tokens
            ),
            (600, 200, 680, 360)
        );
        assert_eq!(
            (total.cache_write_5m_tokens, total.cache_write_1h_tokens),
            (Some(80), Some(120))
        );
        assert_eq!(
            total.cache_read_modalities.unwrap(),
            okapi_domain::CacheModalities {
                audio_tokens: 300,
                image_tokens: 200
            }
        );
        assert_eq!(
            total.cache_write_modalities.unwrap(),
            okapi_domain::CacheModalities {
                audio_tokens: 20,
                image_tokens: 40
            }
        );
        assert_eq!(total.reported_details, u.reported_details);
        assert_eq!(
            (total.prompt_source(), total.completion_source()),
            ("upstream", "upstream")
        );
        assert_eq!(total.total_raw(), 2800);
        assert_eq!(
            total.prompt_uncached()
                + total.cached_text()
                + total.cache_write_text()
                + total.audio_prompt_tokens
                + total.image_prompt_tokens
                + total.cache_read_modalities.unwrap().audio_tokens
                + total.cache_read_modalities.unwrap().image_tokens
                + total.cache_write_modalities.unwrap().audio_tokens
                + total.cache_write_modalities.unwrap().image_tokens,
            2000
        );
    }

    #[test]
    fn missing_batch_usage_is_unknown_and_invalid_counters_cannot_be_zero() {
        let known = usage(Some(
            &json!({"promptTokenCount":100,"candidatesTokenCount":50,"thoughtsTokenCount":0}),
        ))
        .unwrap();
        for entries in [vec![json!({}), json!(known)], vec![json!(known), json!({})]] {
            let total = total_usage(entries).unwrap();
            assert_eq!(total.total_raw(), 150);
            assert!(total.reported_details.is_none() && total.upstream_usage.is_none());
        }
        assert_eq!(total_usage(vec![]).unwrap(), TokenUsage::default());
        for raw in [
            json!({}),
            json!({"promptTokenCount":1}),
            json!({"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":3}),
            json!({"promptTokenCount":-1,"candidatesTokenCount":1}),
        ] {
            assert!(usage(Some(&raw)).is_err(), "{raw}");
        }
        let huge = TokenUsage {
            prompt_tokens: i32::MAX as u32,
            ..TokenUsage::default()
        };
        assert!(
            total_usage(vec![
                json!(huge),
                json!(TokenUsage {
                    prompt_tokens: 1,
                    ..TokenUsage::default()
                })
            ])
            .is_err()
        );
    }
}
