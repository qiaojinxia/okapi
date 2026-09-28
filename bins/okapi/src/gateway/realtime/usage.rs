//! Per-response Realtime usage. Cache counts intersect the modality counts.
use okapi_domain::{CacheModalities, TokenUsage};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct Meter {
    pub usage: TokenUsage,
    pub responses: u32,
    seen: HashSet<String>,
}

impl Meter {
    pub fn observe(&mut self, text: &str) -> Result<(), &'static str> {
        let Ok(event) = serde_json::from_str::<Value>(text) else {
            return Ok(());
        };
        if event["type"] != "response.done" {
            return Ok(());
        }
        let response = &event["response"];
        let id = response["id"].as_str().filter(|id| !id.is_empty());
        if id.is_some_and(|id| self.seen.contains(id)) {
            return Ok(());
        }
        // Bound bookkeeping for compatible servers that never close a session.
        if self.responses >= 65_536 || id.is_some_and(|id| id.len() > 1024) {
            return Err("realtime_usage_limit");
        }
        let raw: RawUsage = serde_json::from_value(response["usage"].clone())
            .map_err(|_| "invalid_realtime_usage")?;
        let next = raw.normalize().ok_or("invalid_realtime_usage")?;
        let total = if self.responses == 0 {
            next
        } else {
            combine(self.usage, next).ok_or("invalid_realtime_usage")?
        };
        // Do not mutate the verified prefix until every field has passed validation.
        self.usage = total;
        self.responses += 1;
        if let Some(id) = id {
            self.seen.insert(id.to_owned());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: u32,
    output_tokens: u32,
    total_tokens: Option<u64>,
    #[serde(default)]
    input_token_details: InputDetails,
    #[serde(default)]
    output_token_details: ModalDetails,
}

#[derive(Default, Deserialize)]
struct InputDetails {
    #[serde(flatten)]
    modalities: ModalDetails,
    cached_tokens: Option<u32>,
    cached_tokens_details: Option<ModalDetails>,
    cache_write_tokens: Option<u32>,
    cache_write_tokens_details: Option<ModalDetails>,
}

#[derive(Default, Deserialize)]
#[allow(clippy::struct_field_names)]
struct ModalDetails {
    text_tokens: Option<u32>,
    audio_tokens: Option<u32>,
    image_tokens: Option<u32>,
}

impl ModalDetails {
    fn split(&self, total: u32) -> Option<(u32, CacheModalities)> {
        let modalities = CacheModalities {
            audio_tokens: self.audio_tokens.unwrap_or(0),
            image_tokens: self.image_tokens.unwrap_or(0),
        };
        let text = total
            .checked_sub(modalities.audio_tokens)?
            .checked_sub(modalities.image_tokens)?;
        if self.text_tokens.is_some_and(|reported| reported != text) {
            return None;
        }
        Some((text, modalities))
    }
}

fn cache(
    total: Option<u32>,
    details: Option<&ModalDetails>,
    text: u32,
    input: CacheModalities,
) -> Option<(u32, Option<CacheModalities>, bool)> {
    let Some(total) = total else {
        return details.is_none().then_some((0, None, false));
    };
    let (cached_text, cached) = if let Some(details) = details {
        if details.text_tokens.is_none()
            && details.audio_tokens.is_none()
            && details.image_tokens.is_none()
        {
            return None;
        }
        details.split(total)?
    } else if total == 0 || input.total_modal() == 0 {
        (total, CacheModalities::default())
    } else if u64::from(total) == u64::from(text) + input.total_modal() {
        (text, input)
    } else if text == 0 && (input.audio_tokens == 0 || input.image_tokens == 0) {
        (
            0,
            CacheModalities {
                audio_tokens: if input.audio_tokens > 0 { total } else { 0 },
                image_tokens: if input.image_tokens > 0 { total } else { 0 },
            },
        )
    } else {
        // A positive mixed-modal cache cannot be assigned an arbitrary text price.
        return None;
    };
    (cached_text <= text
        && cached.audio_tokens <= input.audio_tokens
        && cached.image_tokens <= input.image_tokens)
        .then_some((total, Some(cached), true))
}

impl RawUsage {
    fn normalize(self) -> Option<TokenUsage> {
        if i32::try_from(self.input_tokens).is_err()
            || i32::try_from(self.output_tokens).is_err()
            || self.total_tokens.is_some_and(|total| {
                total != u64::from(self.input_tokens) + u64::from(self.output_tokens)
            })
        {
            return None;
        }
        let details = self.input_token_details;
        let (text, input) = details.modalities.split(self.input_tokens)?;
        let (_, output) = self.output_token_details.split(self.output_tokens)?;
        let (cached_tokens, cache_read_modalities, cache_read_reported) = cache(
            details.cached_tokens,
            details.cached_tokens_details.as_ref(),
            text,
            input,
        )?;
        let (cache_write_tokens, cache_write_modalities, cache_write_reported) = cache(
            details.cache_write_tokens,
            details.cache_write_tokens_details.as_ref(),
            text,
            input,
        )?;
        let read = cache_read_modalities.unwrap_or_default();
        let write = cache_write_modalities.unwrap_or_default();
        let usage = TokenUsage {
            prompt_tokens: self.input_tokens,
            completion_tokens: self.output_tokens,
            cached_tokens,
            cache_write_tokens,
            cache_read_modalities,
            cache_write_modalities,
            cache_read_reported,
            cache_write_reported,
            audio_prompt_tokens: input
                .audio_tokens
                .checked_sub(read.audio_tokens)?
                .checked_sub(write.audio_tokens)?,
            image_prompt_tokens: input
                .image_tokens
                .checked_sub(read.image_tokens)?
                .checked_sub(write.image_tokens)?,
            audio_completion_tokens: output.audio_tokens,
            image_completion_tokens: output.image_tokens,
            ..TokenUsage::default()
        };
        usage.validate().ok()?;
        Some(usage)
    }
}

fn combine(a: TokenUsage, b: TokenUsage) -> Option<TokenUsage> {
    let add = |a: u32, b: u32| a.checked_add(b).filter(|v| i32::try_from(*v).is_ok());
    let modalities = |a: Option<CacheModalities>, b: Option<CacheModalities>| {
        if a.is_none() && b.is_none() {
            return Some(None);
        }
        Some(Some(CacheModalities {
            audio_tokens: add(
                a.unwrap_or_default().audio_tokens,
                b.unwrap_or_default().audio_tokens,
            )?,
            image_tokens: add(
                a.unwrap_or_default().image_tokens,
                b.unwrap_or_default().image_tokens,
            )?,
        }))
    };
    let usage = TokenUsage {
        // Realtime source coverage is tracked separately from this chat contract.
        upstream_usage: None,
        prompt_tokens: add(a.prompt_tokens, b.prompt_tokens)?,
        completion_tokens: add(a.completion_tokens, b.completion_tokens)?,
        cached_tokens: add(a.cached_tokens, b.cached_tokens)?,
        cache_write_tokens: add(a.cache_write_tokens, b.cache_write_tokens)?,
        cache_read_modalities: modalities(a.cache_read_modalities, b.cache_read_modalities)?,
        cache_write_modalities: modalities(a.cache_write_modalities, b.cache_write_modalities)?,
        cache_read_reported: a.cache_read_reported && b.cache_read_reported,
        cache_write_reported: a.cache_write_reported && b.cache_write_reported,
        audio_prompt_tokens: add(a.audio_prompt_tokens, b.audio_prompt_tokens)?,
        image_prompt_tokens: add(a.image_prompt_tokens, b.image_prompt_tokens)?,
        audio_completion_tokens: add(a.audio_completion_tokens, b.audio_completion_tokens)?,
        image_completion_tokens: add(a.image_completion_tokens, b.image_completion_tokens)?,
        reasoning_tokens: add(a.reasoning_tokens, b.reasoning_tokens)?,
    };
    usage.validate().ok()?;
    Some(usage)
}

#[cfg(test)]
mod tests;
