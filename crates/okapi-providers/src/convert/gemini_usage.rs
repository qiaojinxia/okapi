//! Gemini usageMetadata: totals include cached input and thinking output.
use okapi_api::{CompletionTokensDetails, ModalTokensDetails, PromptTokensDetails, UsageProbe};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    prompt_token_count: Option<u32>,
    #[serde(default)]
    candidates_token_count: Option<u32>,
    #[serde(default)]
    thoughts_token_count: u32,
    cached_content_token_count: Option<u32>,
    total_token_count: Option<u64>,
    prompt_tokens_details: Option<Vec<Modality>>,
    cache_tokens_details: Option<Vec<Modality>>,
    candidates_tokens_details: Option<Vec<Modality>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Modality {
    modality: String,
    token_count: u32,
}

fn modalities(rows: Option<&[Modality]>, total: u32, complete: bool) -> Option<(u32, u32)> {
    let mut audio = 0;
    let mut image = 0;
    let mut counted = 0_u64;
    let mut seen = HashSet::new();
    for row in rows.into_iter().flatten() {
        if !seen.insert(row.modality.as_str()) {
            return None;
        }
        counted += u64::from(row.token_count);
        match row.modality.as_str() {
            "AUDIO" => audio = row.token_count,
            "IMAGE" => image = row.token_count,
            // Text and other modalities retain the provider's base input/output price.
            _ => {}
        }
    }
    if counted > u64::from(total) || (complete && counted != u64::from(total)) {
        return None;
    }
    Some((audio, image))
}

fn normalize(meta: &Metadata) -> Option<UsageProbe> {
    let completion = meta.candidates_token_count.map_or(Some(None), |n| {
        n.checked_add(meta.thoughts_token_count).map(Some)
    })?;
    let mut probe: UsageProbe = serde_json::from_value(serde_json::json!({
        "prompt_tokens": meta.prompt_token_count,
        "completion_tokens": completion,
        "total_tokens": meta.total_token_count,
    }))
    .ok()?;
    let (audio, image) = modalities(
        meta.prompt_tokens_details.as_deref(),
        if probe.missing_prompt {
            u32::MAX
        } else {
            probe.prompt_tokens
        },
        false,
    )?;
    let (audio_out, image_out) = modalities(
        meta.candidates_tokens_details.as_deref(),
        if probe.missing_completion {
            u32::MAX
        } else {
            probe
                .completion_tokens
                .checked_sub(meta.thoughts_token_count)?
        },
        false,
    )?;
    let cached_details = if let Some(rows) = meta.cache_tokens_details.as_deref() {
        let total = meta.cached_content_token_count?;
        let (audio, image) = modalities(Some(rows), total, true)?;
        Some(ModalTokensDetails {
            text_tokens: Some(total.checked_sub(audio)?.checked_sub(image)?),
            audio_tokens: Some(audio),
            image_tokens: Some(image),
        })
    } else {
        None
    };
    probe.prompt_tokens_details = PromptTokensDetails {
        cached_tokens: meta.cached_content_token_count.unwrap_or(0),
        cache_read_reported: meta.cached_content_token_count.is_some(),
        audio_tokens: audio,
        image_tokens: image,
        cached_tokens_details: cached_details,
        ..PromptTokensDetails::default()
    };
    probe.completion_tokens_details = CompletionTokensDetails {
        reasoning_tokens: meta.thoughts_token_count,
        audio_tokens: audio_out,
        image_tokens: image_out,
    };
    Some(probe)
}

pub(super) fn parse(meta: Option<&Value>) -> Option<UsageProbe> {
    let meta = meta.filter(|v| !v.is_null())?;
    Some(
        serde_json::from_value::<Metadata>(meta.clone())
            .ok()
            .as_ref()
            .and_then(normalize)
            .unwrap_or_else(UsageProbe::invalid),
    )
}
