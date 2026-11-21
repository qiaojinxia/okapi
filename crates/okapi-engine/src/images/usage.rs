//! Direct Images API usage is per response, never per returned image.
use super::AppError;
use axum::http::StatusCode;
use okapi_domain::{
    CacheModalities, ModalitiesReported, TokenDetailsReported, TokenUsage, UpstreamTokenCounts,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Envelope {
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    total_tokens: Option<u64>,
    input_tokens_details: Option<InputDetails>,
    output_tokens_details: Option<ModalDetails>,
}

#[derive(Deserialize)]
struct InputDetails {
    text_tokens: Option<u32>,
    image_tokens: Option<u32>,
    audio_tokens: Option<u32>,
    cached_tokens: Option<u32>,
    cached_tokens_details: Option<ModalDetails>,
    #[serde(alias = "cache_creation_input_tokens")]
    cache_write_tokens: Option<u32>,
    cache_write_tokens_details: Option<ModalDetails>,
}

#[derive(Deserialize)]
#[allow(clippy::struct_field_names)] // Preserve the upstream wire field names.
struct ModalDetails {
    text_tokens: Option<u32>,
    image_tokens: Option<u32>,
    audio_tokens: Option<u32>,
}

/// Resolve only unique splits. Mixed-modal cache totals without a split are ambiguous.
fn cache_split(
    total: Option<u32>,
    details: Option<&ModalDetails>,
    text: u32,
    image: u32,
) -> Result<(u32, Option<CacheModalities>, bool), AppError> {
    let reported = total.is_some() || details.is_some();
    if !reported {
        return Ok((0, None, false));
    }
    if details
        .as_ref()
        .is_some_and(|v| v.audio_tokens.is_some_and(|v| v != 0))
    {
        return Err(invalid());
    }
    let known_text = details.as_ref().and_then(|v| v.text_tokens);
    let known_image = details.as_ref().and_then(|v| v.image_tokens);
    let total = total
        .or_else(|| known_text?.checked_add(known_image?))
        .ok_or_else(invalid)?;
    let image_cached = match (known_text, known_image) {
        (Some(t), Some(i)) if t.checked_add(i) == Some(total) => i,
        (Some(t), None) => total.checked_sub(t).ok_or_else(invalid)?,
        (None, Some(i)) => i,
        (None, None) if total == 0 || image == 0 => 0,
        (None, None) if text == 0 => total,
        (None, None) if text.checked_add(image) == Some(total) => image,
        _ => return Err(invalid()),
    };
    let text_cached = total.checked_sub(image_cached).ok_or_else(invalid)?;
    if text_cached > text || image_cached > image {
        return Err(invalid());
    }
    Ok((
        total,
        Some(CacheModalities {
            audio_tokens: 0,
            image_tokens: image_cached,
        }),
        true,
    ))
}

fn image_output(total: u32, details: Option<ModalDetails>) -> Result<u32, AppError> {
    let Some(details) = details else {
        return Ok(total);
    };
    if details.audio_tokens.is_some_and(|v| v != 0) {
        return Err(invalid());
    }
    match (details.text_tokens, details.image_tokens) {
        (Some(t), Some(i)) if t.checked_add(i) == Some(total) => Ok(i),
        (Some(t), None) => total.checked_sub(t).ok_or_else(invalid),
        (None, Some(i)) if i <= total => Ok(i),
        _ => Err(invalid()),
    }
}

pub(super) fn invalid() -> AppError {
    AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
        .with_param("invalid_image_usage")
}

pub(super) fn parse(body: &[u8], required: bool) -> Result<Option<TokenUsage>, AppError> {
    let envelope: Envelope = serde_json::from_slice(body).map_err(|_| invalid())?;
    let Some(usage) = envelope.usage else {
        return if required { Err(invalid()) } else { Ok(None) };
    };
    let (Some(input), Some(output), Some(details)) = (
        usage.input_tokens,
        usage.output_tokens,
        usage.input_tokens_details,
    ) else {
        return if required { Err(invalid()) } else { Ok(None) };
    };
    let (Some(text), Some(image)) = (details.text_tokens, details.image_tokens) else {
        return if required { Err(invalid()) } else { Ok(None) };
    };
    if text.checked_add(image) != Some(input)
        || details.audio_tokens.is_some_and(|tokens| tokens != 0)
        || usage.total_tokens.is_some_and(|total| {
            total != u64::from(input) + u64::from(output)
        })
        // PG/CH usage columns must preserve exact values, not silently saturate.
        || input > i32::MAX as u32
        || output > i32::MAX as u32
    {
        return Err(invalid());
    }
    let (cached_tokens, cache_read_modalities, cache_read_reported) = cache_split(
        details.cached_tokens,
        details.cached_tokens_details.as_ref(),
        text,
        image,
    )?;
    let (cache_write_tokens, cache_write_modalities, cache_write_reported) = cache_split(
        details.cache_write_tokens,
        details.cache_write_tokens_details.as_ref(),
        text,
        image,
    )?;
    let cached_image = cache_read_modalities.unwrap_or_default().image_tokens;
    let written_image = cache_write_modalities.unwrap_or_default().image_tokens;
    let image_prompt_tokens = image
        .checked_sub(cached_image)
        .and_then(|v| v.checked_sub(written_image))
        .ok_or_else(invalid)?;
    let usage = TokenUsage {
        upstream_usage: Some(UpstreamTokenCounts {
            prompt_tokens: Some(input),
            completion_tokens: Some(output),
        }),
        reported_details: Some(TokenDetailsReported {
            prompt: ModalitiesReported {
                audio: true,
                image: true,
            },
            // Image API output defaults to image units under this route's contract.
            completion: ModalitiesReported {
                audio: true,
                image: true,
            },
            cache_read: ModalitiesReported {
                audio: cache_read_reported,
                image: cache_read_reported,
            },
            cache_write: ModalitiesReported {
                audio: cache_write_reported,
                image: cache_write_reported,
            },
            reasoning: false,
        }),
        prompt_tokens: input,
        image_prompt_tokens,
        cached_tokens,
        cache_write_tokens,
        cache_read_modalities,
        cache_write_modalities,
        cache_read_reported,
        cache_write_reported,
        completion_tokens: output,
        image_completion_tokens: image_output(output, usage.output_tokens_details)?,
        ..TokenUsage::default()
    };
    usage.validate().map_err(|_| invalid())?;
    Ok(Some(usage))
}

pub(super) fn annotate(snapshot: &mut serde_json::Value, usage: Option<TokenUsage>) {
    snapshot["image_usage_reported"] = serde_json::json!(usage.is_some());
    if let Some(usage) = usage {
        let read = usage.cache_read_modalities.unwrap_or_default();
        let write = usage.cache_write_modalities.unwrap_or_default();
        snapshot["image_output_tokens"] = serde_json::json!(usage.image_completion_tokens);
        snapshot["image_usage"] = serde_json::json!({
            "input_text_tokens": usage.prompt_uncached() + usage.cached_text() + usage.cache_write_text(),
            "input_image_tokens": usage.image_prompt_tokens + read.image_tokens + write.image_tokens,
            "output_image_tokens": usage.image_completion_tokens,
        });
        if usage.text_completion() > 0 {
            snapshot["image_usage"]["output_text_tokens"] =
                serde_json::json!(usage.text_completion());
        }
        if usage.cache_read_reported || usage.cache_write_reported {
            snapshot["image_cache_usage"] = serde_json::json!({
                "read_reported": usage.cache_read_reported, "write_reported": usage.cache_write_reported,
                "read_text_tokens": usage.cached_text(), "read_image_tokens": read.image_tokens,
                "write_text_tokens": usage.cache_write_text(), "write_image_tokens": write.image_tokens,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unique_cache_totals_resolve_without_assuming_mixed_cache_is_text() {
        for (text, image, cached, cached_image) in
            [(10, 0, 5, 0), (0, 10, 5, 5), (4, 6, 10, 6), (4, 6, 0, 0)]
        {
            let body = json!({"usage":{"input_tokens":10,"output_tokens":1,
                "input_tokens_details":{"text_tokens":text,"image_tokens":image,"cached_tokens":cached}}});
            let usage = parse(body.to_string().as_bytes(), true).unwrap().unwrap();
            assert_eq!(usage.cached_tokens, cached);
            assert_eq!(
                usage.cache_read_modalities.unwrap().image_tokens,
                cached_image
            );
            assert!(usage.cache_read_reported);
        }
    }

    #[test]
    fn output_modalities_validate_totals_and_preserve_text_remainders() {
        for details in [
            json!({"image_tokens":80}),
            json!({"text_tokens":20}),
            json!({"text_tokens":20,"image_tokens":80}),
        ] {
            let body = json!({"usage":{"input_tokens":0,"output_tokens":100,
                "input_tokens_details":{"text_tokens":0,"image_tokens":0},"output_tokens_details":details}});
            let usage = parse(body.to_string().as_bytes(), true).unwrap().unwrap();
            assert_eq!(usage.image_completion_tokens, 80);
            assert_eq!(usage.text_completion(), 20);
        }
        for details in [
            json!({}),
            json!({"image_tokens":101}),
            json!({"text_tokens":101}),
            json!({"text_tokens":20,"image_tokens":81}),
            json!({"image_tokens":80,"audio_tokens":1}),
        ] {
            let body = json!({"usage":{"input_tokens":0,"output_tokens":100,
                "input_tokens_details":{"text_tokens":0,"image_tokens":0},"output_tokens_details":details}});
            assert!(parse(body.to_string().as_bytes(), true).is_err());
        }
    }

    #[test]
    fn duplicate_nested_cache_or_output_fields_are_not_last_value_wins() {
        for body in [
            r#"{"usage":{"input_tokens":10,"output_tokens":1,"input_tokens_details":{"text_tokens":5,"image_tokens":5,"cached_tokens":5,"cached_tokens_details":{"image_tokens":2,"image_tokens":3}}}}"#,
            r#"{"usage":{"input_tokens":10,"output_tokens":1,"input_tokens_details":{"text_tokens":5,"image_tokens":5},"output_tokens_details":{"image_tokens":0,"image_tokens":1}}}"#,
        ] {
            assert!(parse(body.as_bytes(), true).is_err());
        }
    }
}
