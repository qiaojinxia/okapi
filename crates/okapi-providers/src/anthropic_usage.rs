//! Anthropic input excludes cache reads/writes; message deltas report cumulative usage.
use okapi_api::{CompletionTokensDetails, PromptTokensDetails, UsageProbe};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Default, Deserialize)]
struct Counters {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    cache_creation_input_tokens: Option<u32>,
    cache_creation: Option<CacheCreation>,
    output_tokens_details: Option<OutputDetails>,
}

#[derive(Clone, Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: u32,
    #[serde(default)]
    ephemeral_5m_input_tokens: u32,
}

#[derive(Clone, Deserialize)]
struct OutputDetails {
    thinking_tokens: u32,
}

impl Counters {
    fn validate(&self) -> Option<()> {
        let prompt = self
            .input_tokens
            .unwrap_or(0)
            .checked_add(self.cache_read_input_tokens.unwrap_or(0))?
            .checked_add(self.cache_creation_input_tokens.unwrap_or(0))?;
        i32::try_from(prompt).ok()?;
        if let Some(output) = self.output_tokens {
            i32::try_from(output).ok()?;
            if self
                .output_tokens_details
                .as_ref()
                .is_some_and(|d| d.thinking_tokens > output)
            {
                return None;
            }
        }
        if let Some(cache) = &self.cache_creation {
            let total = cache
                .ephemeral_1h_input_tokens
                .checked_add(cache.ephemeral_5m_input_tokens)?;
            if total != self.cache_creation_input_tokens? {
                return None;
            }
        }
        Some(())
    }

    fn probe(&self) -> Option<UsageProbe> {
        self.validate()?;
        let prompt = self
            .input_tokens
            .unwrap_or(0)
            .checked_add(self.cache_read_input_tokens.unwrap_or(0))?
            .checked_add(self.cache_creation_input_tokens.unwrap_or(0))?;
        if self.input_tokens.is_none() && self.output_tokens.is_none() {
            return None;
        }
        Some(UsageProbe {
            missing_prompt: self.input_tokens.is_none(),
            missing_completion: self.output_tokens.is_none(),
            prompt_tokens: prompt,
            completion_tokens: self.output_tokens.unwrap_or(0),
            prompt_tokens_details: PromptTokensDetails {
                cached_tokens: self.cache_read_input_tokens.unwrap_or(0),
                cache_write_tokens: self.cache_creation_input_tokens.unwrap_or(0),
                cache_write_5m_tokens: self
                    .cache_creation
                    .as_ref()
                    .map(|c| c.ephemeral_5m_input_tokens),
                cache_write_1h_tokens: self
                    .cache_creation
                    .as_ref()
                    .map(|c| c.ephemeral_1h_input_tokens),
                cache_read_reported: self.cache_read_input_tokens.is_some(),
                cache_write_reported: self.cache_creation_input_tokens.is_some(),
                ..Default::default()
            },
            completion_tokens_details: CompletionTokensDetails {
                reasoning_reported: self.output_tokens_details.is_some(),
                reasoning_tokens: self
                    .output_tokens_details
                    .as_ref()
                    .map_or(0, |d| d.thinking_tokens),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn merge(&mut self, next: Self) -> Option<()> {
        if self
            .output_tokens
            .zip(next.output_tokens)
            .is_some_and(|(before, after)| after < before)
        {
            return None;
        }
        self.input_tokens = next.input_tokens.or(self.input_tokens);
        self.output_tokens = next.output_tokens.or(self.output_tokens);
        self.cache_read_input_tokens = next
            .cache_read_input_tokens
            .or(self.cache_read_input_tokens);
        // A newer aggregate without a TTL split makes the older split stale.
        if next.cache_creation_input_tokens.is_some() || next.cache_creation.is_some() {
            self.cache_creation = next.cache_creation;
        }
        self.cache_creation_input_tokens = next
            .cache_creation_input_tokens
            .or(self.cache_creation_input_tokens);
        self.output_tokens_details = next
            .output_tokens_details
            .or(self.output_tokens_details.take());
        self.validate()
    }
}

fn counters(value: &Value) -> Option<Counters> {
    let counters: Counters = serde_json::from_value(value.clone()).ok()?;
    if counters.input_tokens.is_none()
        && counters.output_tokens.is_none()
        && counters.cache_read_input_tokens.is_none()
        && counters.cache_creation_input_tokens.is_none()
        && counters.output_tokens_details.is_none()
        && counters.cache_creation.is_none()
    {
        return None;
    }
    Some(counters)
}

pub(crate) fn parse(value: Option<&Value>) -> Option<UsageProbe> {
    let value = value.filter(|v| !v.is_null())?;
    Some(
        counters(value)
            .and_then(|c| c.probe())
            .unwrap_or_else(UsageProbe::invalid),
    )
}

#[derive(Default)]
pub(crate) struct StreamUsage {
    counters: Counters,
    started: bool,
    delta_output: bool,
    invalid: bool,
}

impl StreamUsage {
    pub(crate) fn observe(&mut self, event: &str, data: &Value) -> Option<UsageProbe> {
        if !matches!(event, "message_start" | "message_delta" | "message_stop") {
            return None;
        }
        if self.update(event, data).is_none() {
            self.invalid = true;
        }
        if self.invalid {
            return Some(UsageProbe::invalid());
        }
        // Initial output is provisional, but input/cache counts are already useful.
        // Preserve them if the client disconnects before final output usage arrives.
        let mut counts = self.counters.clone();
        if !self.delta_output {
            counts.output_tokens = None;
        }
        counts.probe()
    }

    fn update(&mut self, event: &str, data: &Value) -> Option<()> {
        data.as_object()?;
        let usage = match event {
            "message_start" => {
                if self.started {
                    return None;
                }
                self.started = true;
                data.get("message")?.as_object()?.get("usage")
            }
            "message_delta" => data.get("usage"),
            _ => return Some(()),
        };
        if let Some(usage) = usage.filter(|v| !v.is_null()) {
            let next = counters(usage)?;
            self.delta_output |= event == "message_delta" && next.output_tokens.is_some();
            self.counters.merge(next)?;
        }
        Some(())
    }
}
