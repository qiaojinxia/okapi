//! Native usage objects retained by OpenAI-compatible bridges. This does not
//! add native inference endpoints; it normalizes observations on existing paths.
use crate::{CompletionTokensDetails, ModalTokensDetails, PromptTokensDetails};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

const BEDROCK: &[&str] = &[
    "inputTokens",
    "outputTokens",
    "totalTokens",
    "cacheReadInputTokens",
    "cacheWriteInputTokens",
    "cacheDetails",
];
const INTERACTIONS: &[&str] = &[
    "total_input_tokens",
    "total_output_tokens",
    "total_thought_tokens",
    "total_cached_tokens",
    "input_tokens_by_modality",
    "output_tokens_by_modality",
    "cached_tokens_by_modality",
    "total_tool_use_tokens",
];

/// Whether a bridge retained non-null native Bedrock/Interactions usage fields.
#[must_use]
pub fn has_bridged_usage_fields(value: &Value) -> bool {
    BEDROCK
        .iter()
        .chain(INTERACTIONS)
        .any(|key| value.get(key).is_some_and(|v| !v.is_null()))
}

pub(super) fn canonicalize(value: Value) -> Result<Value, ()> {
    if !value.is_object() {
        return Err(());
    }
    let bedrock = BEDROCK
        .iter()
        .any(|key| value.get(key).is_some_and(|v| !v.is_null()));
    let interactions = INTERACTIONS
        .iter()
        .any(|key| value.get(key).is_some_and(|v| !v.is_null()));
    if !bedrock && !interactions {
        return Ok(value);
    }
    let canonical = [
        "prompt_tokens",
        "completion_tokens",
        "prompt_tokens_details",
        "completion_tokens_details",
    ]
    .iter()
    .any(|key| value.get(key).is_some_and(|v| !v.is_null()));
    let native_axes = [
        "inputTokens",
        "outputTokens",
        "totalTokens",
        "total_input_tokens",
        "total_output_tokens",
        "total_thought_tokens",
    ]
    .iter()
    .any(|key| value.get(key).is_some_and(|v| !v.is_null()));
    if canonical && !native_axes {
        return cache_extensions(value);
    }
    // Do not select an interpretation for mixed protocol observations. A bridge
    // must return one complete usage shape or a canonical normalized shape.
    if bedrock && interactions
        || [
            "promptTokenCount",
            "candidatesTokenCount",
            "thoughtsTokenCount",
            "cachedContentTokenCount",
            "totalTokenCount",
            "promptTokensDetails",
            "cacheTokensDetails",
            "candidatesTokensDetails",
            "input_tokens",
            "output_tokens",
            "input_tokens_details",
            "output_tokens_details",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "cache_creation",
            "prompt_tokens",
            "completion_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
            "prompt_cache_hit_tokens",
            "prompt_cache_miss_tokens",
        ]
        .iter()
        .any(|key| value.get(key).is_some_and(|v| !v.is_null()))
    {
        return Err(());
    }
    if bedrock {
        let raw: Bedrock = serde_json::from_value(value).map_err(|_| ())?;
        raw.canonical()
    } else {
        let raw: Interactions = serde_json::from_value(value).map_err(|_| ())?;
        raw.canonical()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bedrock {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    total_tokens: Option<u64>,
    cache_read_input_tokens: Option<u32>,
    cache_write_input_tokens: Option<u32>,
    cache_details: Option<Vec<CacheDetail>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheDetail {
    ttl: String,
    input_tokens: u32,
}

fn lifetimes(rows: Option<Vec<CacheDetail>>, write: Option<u32>) -> Result<Option<(u32, u32)>, ()> {
    let Some(rows) = rows.filter(|rows| !rows.is_empty() || write.is_some()) else {
        return Ok(None);
    };
    let mut seen = HashSet::new();
    let mut short = 0_u32;
    let mut long = 0_u32;
    for row in rows {
        if !seen.insert(row.ttl.clone()) {
            return Err(());
        }
        match row.ttl.as_str() {
            "5m" => short = row.input_tokens,
            "1h" => long = row.input_tokens,
            _ => return Err(()),
        }
    }
    if write != short.checked_add(long) {
        return Err(());
    }
    Ok(Some((short, long)))
}

#[derive(Deserialize)]
struct CacheExtensions {
    #[serde(rename = "cacheReadInputTokens")]
    read: Option<u32>,
    #[serde(rename = "cacheWriteInputTokens")]
    write: Option<u32>,
    #[serde(rename = "cacheDetails")]
    lifetimes: Option<Vec<CacheDetail>>,
    total_cached_tokens: Option<u32>,
    cached_tokens_by_modality: Option<Vec<Modality>>,
}

fn cache_extensions(mut value: Value) -> Result<Value, ()> {
    if [
        "input_tokens_by_modality",
        "output_tokens_by_modality",
        "total_tool_use_tokens",
    ]
    .iter()
    .any(|key| value.get(key).is_some_and(|v| !v.is_null()))
    {
        return Err(());
    }
    let raw: CacheExtensions = serde_json::from_value(value.clone()).map_err(|_| ())?;
    let mut details = value
        .get("prompt_tokens_details")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value::<PromptTokensDetails>(v.clone()).map_err(|_| ()))
        .transpose()?
        .unwrap_or_default();
    let read = crate::chat::cache_counter(&[
        details.cache_read_reported.then_some(details.cached_tokens),
        raw.read,
        raw.total_cached_tokens,
    ])
    .map_err(|_| ())?;
    let write = crate::chat::cache_counter(&[
        details
            .cache_write_reported
            .then_some(details.cache_write_tokens),
        raw.write,
    ])
    .map_err(|_| ())?;
    details.cached_tokens = read.unwrap_or(0);
    details.cache_read_reported = read.is_some();
    details.cache_write_tokens = write.unwrap_or(0);
    details.cache_write_reported = write.is_some();
    if let Some((short, long)) = lifetimes(raw.lifetimes, write)? {
        details.cache_write_5m_tokens =
            crate::chat::cache_counter(&[details.cache_write_5m_tokens, Some(short)])
                .map_err(|_| ())?;
        details.cache_write_1h_tokens =
            crate::chat::cache_counter(&[details.cache_write_1h_tokens, Some(long)])
                .map_err(|_| ())?;
    }
    if let Some(mut modal) = modalities(raw.cached_tokens_by_modality, read, true)? {
        modal.text_tokens = Some(
            read.ok_or(())?
                .checked_sub(modal.audio_tokens.unwrap_or(0))
                .ok_or(())?
                .checked_sub(modal.image_tokens.unwrap_or(0))
                .ok_or(())?,
        );
        if let Some(previous) = details.cached_tokens_details {
            modal.audio_tokens =
                crate::chat::cache_counter(&[modal.audio_tokens, previous.audio_tokens])
                    .map_err(|_| ())?;
            modal.image_tokens =
                crate::chat::cache_counter(&[modal.image_tokens, previous.image_tokens])
                    .map_err(|_| ())?;
            modal.text_tokens =
                crate::chat::cache_counter(&[modal.text_tokens, previous.text_tokens])
                    .map_err(|_| ())?;
        }
        details.cached_tokens_details = Some(modal);
    }
    value["prompt_tokens_details"] = details.cache_json();
    Ok(value)
}

impl Bedrock {
    fn canonical(self) -> Result<Value, ()> {
        // Converse's inputTokens excludes both cache axes. Unlike the OpenAI
        // shape, normalizing it requires adding the observed cache subsets once.
        let prompt = self
            .input_tokens
            .map(|input| {
                input
                    .checked_add(self.cache_read_input_tokens.unwrap_or(0))
                    .and_then(|n| n.checked_add(self.cache_write_input_tokens.unwrap_or(0)))
                    .ok_or(())
            })
            .transpose()?;
        if let Some(total) = self.total_tokens {
            let input = self.input_tokens.ok_or(())?;
            let output = self.output_tokens.ok_or(())?;
            // Bridges may preserve the wire sum or expose the normalized sum.
            // Both are checkable; neither may infer an absent cache observation.
            let wire_total = u64::from(input) + u64::from(output);
            let normalized_total = u64::from(prompt.ok_or(())?) + u64::from(output);
            if total != wire_total && total != normalized_total {
                return Err(());
            }
        }
        let mut details = PromptTokensDetails {
            cached_tokens: self.cache_read_input_tokens.unwrap_or(0),
            cache_read_reported: self.cache_read_input_tokens.is_some(),
            cache_write_tokens: self.cache_write_input_tokens.unwrap_or(0),
            cache_write_reported: self.cache_write_input_tokens.is_some(),
            ..PromptTokensDetails::default()
        };
        if let Some((short, long)) = lifetimes(self.cache_details, self.cache_write_input_tokens)? {
            details.cache_write_5m_tokens = Some(short);
            details.cache_write_1h_tokens = Some(long);
        }
        Ok(json!({
            "prompt_tokens": prompt, "completion_tokens": self.output_tokens,
            "prompt_tokens_details": details.cache_json(),
        }))
    }
}

#[derive(Deserialize)]
struct Interactions {
    total_input_tokens: Option<u32>,
    total_output_tokens: Option<u32>,
    total_thought_tokens: Option<u32>,
    total_cached_tokens: Option<u32>,
    total_tokens: Option<u64>,
    // This is already reflected in the provider's totals, not a second charge.
    #[serde(rename = "total_tool_use_tokens")]
    _total_tool_use_tokens: Option<u32>,
    input_tokens_by_modality: Option<Vec<Modality>>,
    output_tokens_by_modality: Option<Vec<Modality>>,
    cached_tokens_by_modality: Option<Vec<Modality>>,
}

#[derive(Deserialize)]
struct Modality {
    modality: String,
    tokens: u32,
}

fn modalities(
    rows: Option<Vec<Modality>>,
    total: Option<u32>,
    complete: bool,
) -> Result<Option<ModalTokensDetails>, ()> {
    let Some(rows) = rows else {
        return Ok(None);
    };
    let mut seen = HashSet::new();
    let mut sum = 0_u64;
    let mut details = ModalTokensDetails::default();
    for row in rows {
        if !seen.insert(row.modality.clone()) {
            return Err(());
        }
        sum += u64::from(row.tokens);
        match row.modality.as_str() {
            "audio" => details.audio_tokens = Some(row.tokens),
            "image" => details.image_tokens = Some(row.tokens),
            "text" | "video" | "document" => {}
            _ => return Err(()),
        }
    }
    if complete && total.map(u64::from) != Some(sum) {
        return Err(());
    }
    if let Some(total) = total {
        if sum > u64::from(total) {
            return Err(());
        }
        if sum == u64::from(total) {
            details.audio_tokens.get_or_insert(0);
            details.image_tokens.get_or_insert(0);
        }
    }
    Ok(Some(details))
}

impl Interactions {
    fn canonical(self) -> Result<Value, ()> {
        let completion = self
            .total_output_tokens
            .map(|output| {
                output
                    .checked_add(self.total_thought_tokens.unwrap_or(0))
                    .ok_or(())
            })
            .transpose()?;
        let input = modalities(
            self.input_tokens_by_modality,
            self.total_input_tokens,
            false,
        )?;
        let output = modalities(
            self.output_tokens_by_modality,
            self.total_output_tokens,
            false,
        )?;
        // Partial cache rows cannot establish the remainder's billing modality.
        let cached = modalities(
            self.cached_tokens_by_modality,
            self.total_cached_tokens,
            true,
        )?;
        let d = input.unwrap_or_default();
        let c = output.unwrap_or_default();
        let mut cached = cached;
        if let Some(details) = &mut cached {
            details.text_tokens = Some(
                self.total_cached_tokens
                    .ok_or(())?
                    .checked_sub(details.audio_tokens.unwrap_or(0))
                    .ok_or(())?
                    .checked_sub(details.image_tokens.unwrap_or(0))
                    .ok_or(())?,
            );
        }
        let details = PromptTokensDetails {
            cached_tokens: self.total_cached_tokens.unwrap_or(0),
            cache_read_reported: self.total_cached_tokens.is_some(),
            audio_tokens: d.audio_tokens.unwrap_or(0),
            image_tokens: d.image_tokens.unwrap_or(0),
            modalities_reported: okapi_domain::ModalitiesReported {
                audio: d.audio_tokens.is_some(),
                image: d.image_tokens.is_some(),
            },
            cached_tokens_details: cached,
            ..PromptTokensDetails::default()
        };
        let output_details = CompletionTokensDetails {
            audio_tokens: c.audio_tokens.unwrap_or(0),
            image_tokens: c.image_tokens.unwrap_or(0),
            modalities_reported: okapi_domain::ModalitiesReported {
                audio: c.audio_tokens.is_some(),
                image: c.image_tokens.is_some(),
            },
            reasoning_tokens: self.total_thought_tokens.unwrap_or(0),
            reasoning_reported: self.total_thought_tokens.is_some(),
        };
        Ok(json!({
            "prompt_tokens": self.total_input_tokens, "completion_tokens": completion,
            "total_tokens": self.total_tokens,
            "prompt_tokens_details": details.cache_json(),
            "completion_tokens_details": output_details.to_json(),
        }))
    }
}
