use crate::{UsageProbe, chat::cache_counter};
use serde_json::{Value, json};

fn read_u32_field(object: &Value, key: &str) -> Result<Option<u32>, ()> {
    let map = object.as_object().ok_or(())?;
    match map.get(key).filter(|v| !v.is_null()) {
        Some(value) => serde_json::from_value::<u32>(value.clone())
            .map(Some)
            .map_err(|_| ()),
        None => Ok(None),
    }
}

fn envelope_parts<'a>(
    raw: Option<Value>,
    timings: Option<&'a Value>,
    choice_usages: impl IntoIterator<Item = &'a Value>,
) -> Result<Option<UsageProbe>, ()> {
    let mut reads = Vec::new();
    for usage in choice_usages {
        if !usage.is_null()
            && let Some(read) = read_u32_field(usage, "cached_tokens")?
        {
            reads.push(Some(read));
        }
    }
    if let Some(timings) = timings.filter(|v| !v.is_null())
        && let Some(read) = read_u32_field(timings, "cache_n")?
    {
        reads.push(Some(read));
    }
    let read = cache_counter(&reads).map_err(|_| ())?;
    let raw = raw.filter(|v| !v.is_null());
    if raw.is_none() && read.is_none() {
        return Ok(None);
    }
    let mut raw = raw.unwrap_or_else(|| json!({}));
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

fn envelope(value: &Value) -> Result<Option<UsageProbe>, ()> {
    let choice_usages = value
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("usage"));
    envelope_parts(
        value.get("usage").filter(|v| !v.is_null()).cloned(),
        value.get("timings"),
        choice_usages,
    )
}

/// Streaming fast-path counterpart of [`usage_from_chat`] for already-extracted
/// optional usage subtrees, avoiding a full-chunk `serde_json::Value` DOM.
#[must_use]
pub(crate) fn usage_from_chat_parts<'a>(
    raw: Option<Value>,
    timings: Option<&'a Value>,
    choice_usages: impl IntoIterator<Item = &'a Value>,
) -> Option<UsageProbe> {
    envelope_parts(raw, timings, choice_usages).unwrap_or_else(|()| Some(UsageProbe::invalid()))
}

/// Read Chat usage, including Kimi choice usage and llama.cpp timing counters.
/// Invalid reported counters remain invalid rather than falling back to estimates.
#[must_use]
pub fn usage_from_chat(value: &Value) -> Option<UsageProbe> {
    envelope(value).unwrap_or_else(|()| Some(UsageProbe::invalid()))
}
