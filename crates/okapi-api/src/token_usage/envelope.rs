use crate::{UsageProbe, chat::cache_counter};
use serde_json::{Value, json};

fn envelope(value: &Value) -> Result<Option<UsageProbe>, ()> {
    let mut reads = Vec::new();
    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        for choice in choices {
            if let Some(usage) = choice.get("usage").filter(|v| !v.is_null()) {
                let usage = usage.as_object().ok_or(())?;
                if let Some(read) = usage.get("cached_tokens").filter(|v| !v.is_null()) {
                    reads.push(Some(
                        serde_json::from_value::<u32>(read.clone()).map_err(|_| ())?,
                    ));
                }
            }
        }
    }
    if let Some(timings) = value.get("timings").filter(|v| !v.is_null()) {
        let timings = timings.as_object().ok_or(())?;
        if let Some(read) = timings.get("cache_n").filter(|v| !v.is_null()) {
            reads.push(Some(
                serde_json::from_value::<u32>(read.clone()).map_err(|_| ())?,
            ));
        }
    }
    let read = cache_counter(&reads).map_err(|_| ())?;
    let raw = value.get("usage").filter(|v| !v.is_null());
    if raw.is_none() && read.is_none() {
        return Ok(None);
    }
    let mut raw = raw.cloned().unwrap_or_else(|| json!({}));
    if let Some(read) = read {
        let object = raw.as_object_mut().ok_or(())?;
        // Keep a root alias, allowing the common parser to compare it with
        // prompt details. Choice counters describe the request, not a sum of choices.
        if let Some(existing) = object.get("cached_tokens").filter(|v| !v.is_null()) {
            let existing: u32 = serde_json::from_value(existing.clone()).map_err(|_| ())?;
            if existing != read {
                return Err(());
            }
        }
        object.insert("cached_tokens".into(), json!(read));
    }
    Ok(Some(serde_json::from_value(raw).map_err(|_| ())?))
}

/// Read Chat usage, including Kimi choice usage and llama.cpp timing counters.
/// Invalid reported counters remain invalid rather than falling back to estimates.
#[must_use]
pub fn usage_from_chat(value: &Value) -> Option<UsageProbe> {
    envelope(value).unwrap_or_else(|()| Some(UsageProbe::invalid()))
}
