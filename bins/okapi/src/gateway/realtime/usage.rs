//! Per-response Realtime usage. Cache counts intersect the modality counts.
use okapi_domain::{
    CacheModalities, ModalitiesReported, TokenDetailsReported, TokenUsage, UpstreamTokenCounts,
};
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
    fn reported(&self, total: u32) -> ModalitiesReported {
        okapi_api::ModalTokensDetails {
            text_tokens: self.text_tokens,
            audio_tokens: self.audio_tokens,
            image_tokens: self.image_tokens,
        }
        .reported(total)
    }

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
        let input_reported = details.modalities.reported(self.input_tokens);
        let cache_reported = |total: Option<u32>, split: Option<&ModalDetails>| {
            total.map_or(ModalitiesReported::default(), |total| {
                if total == 0 {
                    ModalitiesReported {
                        audio: true,
                        image: true,
                    }
                } else {
                    split.map_or(input_reported, |split| split.reported(total))
                }
            })
        };
        let read_reported = cache_reported(
            details.cached_tokens,
            details.cached_tokens_details.as_ref(),
        );
        let write_reported = cache_reported(
            details.cache_write_tokens,
            details.cache_write_tokens_details.as_ref(),
        );
        let usage = TokenUsage {
            upstream_usage: Some(UpstreamTokenCounts {
                prompt_tokens: Some(self.input_tokens),
                completion_tokens: Some(self.output_tokens),
            }),
            reported_details: Some(TokenDetailsReported {
                prompt: ModalitiesReported {
                    audio: input_reported.audio
                        && (cached_tokens == 0 || read_reported.audio)
                        && (cache_write_tokens == 0 || write_reported.audio),
                    image: input_reported.image
                        && (cached_tokens == 0 || read_reported.image)
                        && (cache_write_tokens == 0 || write_reported.image),
                },
                completion: self.output_token_details.reported(self.output_tokens),
                cache_read: read_reported,
                cache_write: write_reported,
                reasoning: false,
            }),
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
    a.checked_add(b).ok()
}

#[cfg(test)]
mod tests;
