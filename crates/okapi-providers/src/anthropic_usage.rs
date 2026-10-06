//! Anthropic input excludes cache reads/writes; message deltas report cumulative usage.
use okapi_api::{CompletionTokensDetails, PromptTokensDetails, UsageProbe};
use okapi_domain::{AnthropicToolUsage, ServerToolUsage};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Default, Deserialize)]
struct Counters {
    server_tool_use: Option<AnthropicToolUsage>,
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

#[derive(Deserialize)]
struct CompatibleInput {
    prompt_tokens: Option<u32>,
    prompt_cache_miss_tokens: Option<u32>,
}

impl Counters {
    fn validate(&self) -> Option<()> {
        if let Some(tools) = self.server_tool_use {
            tools.validate().ok()?;
        }
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
        if self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.server_tool_use.is_none()
            && self.cache_read_input_tokens.is_none()
            && self.cache_creation_input_tokens.is_none()
        {
            return None;
        }
        Some(UsageProbe {
            server_tool_usage: self.server_tool_use.map(ServerToolUsage::Anthropic),
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
        self.server_tool_use = match (next.server_tool_use, self.server_tool_use) {
            (Some(next), Some(before)) => Some(next.with_previous(before).ok()?),
            (next, before) => next.or(before),
        };
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
        // A newer aggregate without a TTL split makes the older split stale, unless it
        // repeats the same total: Anthropic's final message_delta reports
        // cache_creation_input_tokens without the split message_start already gave.
        let same_total = next.cache_creation.is_none()
            && next.cache_creation_input_tokens.is_some()
            && next.cache_creation_input_tokens == self.cache_creation_input_tokens;
        if !same_total
            && (next.cache_creation_input_tokens.is_some() || next.cache_creation.is_some())
        {
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
    let object = value.as_object()?;
    // Serde structs can accept positional arrays; native usage requires an object.
    if object
        .get("server_tool_use")
        .is_some_and(|tools| !tools.is_null() && !tools.is_object())
    {
        return None;
    }
    let mut counters: Counters = serde_json::from_value(value.clone()).ok()?;
    let details = okapi_api::compatible_cache_details(value).ok()?;
    counters.cache_read_input_tokens = details.cache_read_reported.then_some(details.cached_tokens);
    counters.cache_creation_input_tokens = details
        .cache_write_reported
        .then_some(details.cache_write_tokens);
    match (details.cache_write_5m_tokens, details.cache_write_1h_tokens) {
        (Some(short), Some(long)) => {
            counters.cache_creation = Some(CacheCreation {
                ephemeral_5m_input_tokens: short,
                ephemeral_1h_input_tokens: long,
            });
        }
        (None, None) => {}
        _ => return None,
    }
    let mixed: CompatibleInput = serde_json::from_value(value.clone()).ok()?;
    let mut inclusive = mixed.prompt_tokens;
    if let (Some(read), Some(miss)) = (
        counters.cache_read_input_tokens,
        mixed.prompt_cache_miss_tokens,
    ) {
        let total = read.checked_add(miss)?;
        if inclusive.is_some_and(|prompt| prompt != total) {
            return None;
        }
        inclusive = Some(total);
    } else if mixed
        .prompt_cache_miss_tokens
        .zip(inclusive)
        .is_some_and(|(miss, prompt)| miss > prompt)
    {
        return None;
    }
    if let Some(total) = inclusive {
        // Mixed Messages bridges may mirror inclusive prompt_tokens in
        // input_tokens. Native Anthropic input_tokens otherwise excludes caches.
        let regular = total
            .checked_sub(counters.cache_read_input_tokens.unwrap_or(0))?
            .checked_sub(counters.cache_creation_input_tokens.unwrap_or(0))?;
        if counters
            .input_tokens
            .is_some_and(|input| input != regular && input != total)
        {
            return None;
        }
        counters.input_tokens = Some(regular);
    }
    if counters.input_tokens.is_none()
        && counters.output_tokens.is_none()
        && counters.cache_read_input_tokens.is_none()
        && counters.cache_creation_input_tokens.is_none()
        && counters.output_tokens_details.is_none()
        && counters.cache_creation.is_none()
        && counters.server_tool_use.is_none()
    {
        return None;
    }
    Some(counters)
}

pub(crate) fn parse(value: Option<&Value>) -> Option<UsageProbe> {
    let value = value.filter(|v| !v.is_null())?;
    if okapi_api::has_bridged_usage_fields(value) {
        return Some(
            serde_json::from_value(value.clone()).unwrap_or_else(|_| UsageProbe::invalid()),
        );
    }
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
    bridged: Option<UsageProbe>,
}

impl StreamUsage {
    pub(crate) fn observe(&mut self, event: &str, data: &Value) -> Option<UsageProbe> {
        if !matches!(event, "message_start" | "message_delta" | "message_stop") {
            return None;
        }
        let usage = match event {
            "message_start" => data.pointer("/message/usage"),
            "message_delta" => data.get("usage"),
            _ => None,
        }
        .filter(|v| !v.is_null());
        if self.bridged.is_some() || usage.is_some_and(okapi_api::has_bridged_usage_fields) {
            return self.observe_bridged(event, usage);
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

    fn observe_bridged(&mut self, event: &str, value: Option<&Value>) -> Option<UsageProbe> {
        // An earlier native Anthropic observation cannot be reinterpreted as
        // Bedrock/Interactions halfway through a stream.
        if self.counters.input_tokens.is_some()
            || self.counters.output_tokens.is_some()
            || self.counters.cache_read_input_tokens.is_some()
            || self.counters.cache_creation_input_tokens.is_some()
            || self.counters.server_tool_use.is_some()
        {
            self.invalid = true;
        }
        if event == "message_start" {
            if self.started {
                self.invalid = true;
            }
            self.started = true;
        }
        if let Some(value) = value {
            if !okapi_api::has_bridged_usage_fields(value) {
                self.invalid = true;
            }
            let mut next = parse(Some(value)).unwrap_or_else(UsageProbe::invalid);
            if event == "message_start" {
                next.missing_completion = true;
            }
            self.bridged = Some(next.with_previous(self.bridged));
        }
        if self.invalid {
            return Some(UsageProbe::invalid());
        }
        self.bridged
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

#[cfg(test)]
mod stream_split_tests {
    use super::StreamUsage;
    use serde_json::json;

    /// Real upstream shape: the split arrives only in message_start.
    #[test]
    fn final_delta_keeps_the_ttl_split_it_does_not_restate() {
        let mut usage = StreamUsage::default();
        usage.observe(
            "message_start",
            &json!({"message":{"usage":{"input_tokens":4,"cache_creation_input_tokens":4676,
                "cache_read_input_tokens":0,"output_tokens":1,
                "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":4676}}}}),
        );
        let last = usage
            .observe(
                "message_delta",
                &json!({"usage":{"input_tokens":4,"cache_creation_input_tokens":4676,
                    "cache_read_input_tokens":0,"output_tokens":5}}),
            )
            .unwrap();
        let details = last.prompt_tokens_details;
        assert_eq!(details.cache_write_tokens, 4676);
        assert_eq!(details.cache_write_5m_tokens, Some(0));
        assert_eq!(details.cache_write_1h_tokens, Some(4676));
        assert_eq!(last.completion_tokens, 5);
    }

    /// A changed total without its own split cannot reuse the old one.
    #[test]
    fn changed_total_without_split_drops_the_stale_split() {
        let mut usage = StreamUsage::default();
        usage.observe(
            "message_start",
            &json!({"message":{"usage":{"input_tokens":4,"cache_creation_input_tokens":100,
                "output_tokens":1,
                "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":100}}}}),
        );
        let last = usage
            .observe(
                "message_delta",
                &json!({"usage":{"input_tokens":4,"cache_creation_input_tokens":200,"output_tokens":5}}),
            )
            .unwrap();
        assert_eq!(last.prompt_tokens_details.cache_write_tokens, 200);
        assert_eq!(last.prompt_tokens_details.cache_write_1h_tokens, None);
    }
}
