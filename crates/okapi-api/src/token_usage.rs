use crate::{ModalTokensDetails, UsageProbe};
use okapi_domain::{
    CacheModalities, DomainError, ModalitiesReported, TokenDetailsReported, TokenUsage,
    UpstreamTokenCounts,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct RawUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u64>,
    prompt_tokens_details: Option<crate::PromptTokensDetails>,
    completion_tokens_details: Option<crate::CompletionTokensDetails>,
}

impl<'de> Deserialize<'de> for UsageProbe {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let Ok(mut raw) = serde_json::from_value::<RawUsage>(value) else {
            return Ok(Self::invalid());
        };
        if raw.prompt_tokens.is_none() && raw.completion_tokens.is_none() {
            return Ok(Self::invalid());
        }
        if let Some(total) = raw.total_tokens {
            let inferred = match (raw.prompt_tokens, raw.completion_tokens) {
                (Some(prompt), None) => total.checked_sub(u64::from(prompt)),
                (None, Some(completion)) => total.checked_sub(u64::from(completion)),
                _ => None,
            };
            if raw.prompt_tokens.is_none() || raw.completion_tokens.is_none() {
                let Some(count) = inferred.and_then(|n| u32::try_from(n).ok()) else {
                    return Ok(Self::invalid());
                };
                if raw.prompt_tokens.is_none() {
                    raw.prompt_tokens = Some(count);
                } else {
                    raw.completion_tokens = Some(count);
                }
            }
        }
        let probe = Self {
            missing_prompt: raw.prompt_tokens.is_none(),
            missing_completion: raw.completion_tokens.is_none(),
            prompt_tokens: raw.prompt_tokens.unwrap_or(0),
            completion_tokens: raw.completion_tokens.unwrap_or(0),
            prompt_tokens_details: raw.prompt_tokens_details.unwrap_or_default(),
            completion_tokens_details: raw.completion_tokens_details.unwrap_or_default(),
            invalid: raw.total_tokens.is_some_and(|total| {
                total
                    != u64::from(raw.prompt_tokens.unwrap_or(0))
                        + u64::from(raw.completion_tokens.unwrap_or(0))
            }),
        };
        Ok(probe)
    }
}

fn invalid() -> DomainError {
    DomainError::InvalidTokenUsage {
        reason: "inconsistent_or_ambiguous_modal_usage",
    }
}

fn cache(
    total: u32,
    reported: bool,
    details: Option<ModalTokensDetails>,
    text: u32,
    input: CacheModalities,
) -> Result<Option<CacheModalities>, DomainError> {
    if !reported && total == 0 {
        return if details.is_none() {
            Ok(None)
        } else {
            Err(invalid())
        };
    }
    if total > 0 && details.is_none() && input.total_modal() == 0 {
        // No modal counters were supplied. Preserve the existing base-priced cache
        // without claiming that its modal composition was explicitly observed.
        return Ok(None);
    }
    let (cached_text, cached) = if let Some(details) = details {
        if details.text_tokens.is_none()
            && details.audio_tokens.is_none()
            && details.image_tokens.is_none()
        {
            return Err(invalid());
        }
        let cached = CacheModalities {
            audio_tokens: details.audio_tokens.unwrap_or(0),
            image_tokens: details.image_tokens.unwrap_or(0),
        };
        let remaining = total
            .checked_sub(cached.audio_tokens)
            .and_then(|v| v.checked_sub(cached.image_tokens))
            .ok_or_else(invalid)?;
        if details.text_tokens.is_some_and(|v| v != remaining) {
            return Err(invalid());
        }
        (remaining, cached)
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
        return Err(invalid());
    };
    if cached_text > text
        || cached.audio_tokens > input.audio_tokens
        || cached.image_tokens > input.image_tokens
    {
        return Err(invalid());
    }
    Ok(Some(cached))
}

fn cache_reported(
    total: u32,
    reported: bool,
    details: Option<ModalTokensDetails>,
    input: ModalitiesReported,
) -> ModalitiesReported {
    if !reported && total == 0 {
        return ModalitiesReported::default();
    }
    if total == 0 {
        return ModalitiesReported {
            audio: true,
            image: true,
        };
    }
    let Some(details) = details else {
        return input;
    };
    details.reported(total)
}

fn reported(probe: UsageProbe) -> TokenDetailsReported {
    let d = probe.prompt_tokens_details;
    let c = probe.completion_tokens_details;
    let input = ModalitiesReported {
        audio: d.modalities_reported.audio || d.audio_tokens > 0,
        image: d.modalities_reported.image || d.image_tokens > 0,
    };
    let read = cache_reported(
        d.cached_tokens,
        d.cache_read_reported,
        d.cached_tokens_details,
        input,
    );
    let write = cache_reported(
        d.cache_write_tokens,
        d.cache_write_reported,
        d.cache_write_tokens_details,
        input,
    );
    TokenDetailsReported {
        prompt: ModalitiesReported {
            audio: input.audio
                && (d.cached_tokens == 0 || read.audio)
                && (d.cache_write_tokens == 0 || write.audio),
            image: input.image
                && (d.cached_tokens == 0 || read.image)
                && (d.cache_write_tokens == 0 || write.image),
        },
        completion: ModalitiesReported {
            audio: c.modalities_reported.audio || c.audio_tokens > 0,
            image: c.modalities_reported.image || c.image_tokens > 0,
        },
        cache_read: read,
        cache_write: write,
        reasoning: c.reasoning_reported || c.reasoning_tokens > 0,
    }
}

pub(super) fn normalize(probe: UsageProbe) -> Result<TokenUsage, DomainError> {
    if probe.invalid
        || probe.missing_prompt
        || probe.missing_completion
        || i32::try_from(probe.prompt_tokens).is_err()
        || i32::try_from(probe.completion_tokens).is_err()
    {
        return Err(invalid());
    }
    let d = probe.prompt_tokens_details;
    let text = probe
        .prompt_tokens
        .checked_sub(d.audio_tokens)
        .and_then(|v| v.checked_sub(d.image_tokens))
        .ok_or_else(invalid)?;
    let input = CacheModalities {
        audio_tokens: d.audio_tokens,
        image_tokens: d.image_tokens,
    };
    let read = cache(
        d.cached_tokens,
        d.cache_read_reported,
        d.cached_tokens_details,
        text,
        input,
    )?;
    let write = cache(
        d.cache_write_tokens,
        d.cache_write_reported,
        d.cache_write_tokens_details,
        text,
        input,
    )?;
    let subtract = |total: u32, read: u32, write: u32| {
        total
            .checked_sub(read)
            .and_then(|v| v.checked_sub(write))
            .ok_or_else(invalid)
    };
    let usage = TokenUsage {
        reported_details: Some(reported(probe)),
        upstream_usage: Some(UpstreamTokenCounts {
            prompt_tokens: Some(probe.prompt_tokens),
            completion_tokens: Some(probe.completion_tokens),
        }),
        prompt_tokens: probe.prompt_tokens,
        completion_tokens: probe.completion_tokens,
        cached_tokens: d.cached_tokens,
        cache_write_tokens: d.cache_write_tokens,
        cache_write_5m_tokens: d.cache_write_5m_tokens,
        cache_write_1h_tokens: d.cache_write_1h_tokens,
        cache_read_reported: d.cache_read_reported,
        cache_write_reported: d.cache_write_reported,
        cache_read_modalities: read,
        cache_write_modalities: write,
        audio_prompt_tokens: subtract(
            d.audio_tokens,
            read.unwrap_or_default().audio_tokens,
            write.unwrap_or_default().audio_tokens,
        )?,
        image_prompt_tokens: subtract(
            d.image_tokens,
            read.unwrap_or_default().image_tokens,
            write.unwrap_or_default().image_tokens,
        )?,
        audio_completion_tokens: probe.completion_tokens_details.audio_tokens,
        image_completion_tokens: probe.completion_tokens_details.image_tokens,
        reasoning_tokens: probe.completion_tokens_details.reasoning_tokens,
    };
    usage.validate()?;
    Ok(usage)
}

pub(super) fn with_estimates(
    mut probe: UsageProbe,
    prompt: u32,
    completion: u32,
) -> Result<TokenUsage, DomainError> {
    let upstream = UpstreamTokenCounts {
        prompt_tokens: (!probe.missing_prompt).then_some(probe.prompt_tokens),
        completion_tokens: (!probe.missing_completion).then_some(probe.completion_tokens),
    };
    if probe.missing_prompt {
        let details = probe.prompt_tokens_details;
        // A conservative lower bound keeps known subsets; their intersections are
        // still validated by normalize, never silently clipped.
        let floor = details
            .cached_tokens
            .checked_add(details.cache_write_tokens)
            .and_then(|n| n.checked_add(details.audio_tokens))
            .and_then(|n| n.checked_add(details.image_tokens))
            .ok_or_else(invalid)?;
        probe.prompt_tokens = prompt.max(floor);
        probe.missing_prompt = false;
    }
    if probe.missing_completion {
        let details = probe.completion_tokens_details;
        let floor = details
            .audio_tokens
            .checked_add(details.image_tokens)
            .ok_or_else(invalid)?;
        probe.completion_tokens = completion.max(floor).max(details.reasoning_tokens);
        probe.missing_completion = false;
    }
    let mut usage = normalize(probe)?;
    usage.upstream_usage = Some(upstream);
    Ok(usage)
}
