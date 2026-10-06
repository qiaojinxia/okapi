//! Compatible cache names are mirrors, never additional input tokens.
use crate::PromptTokensDetails;
use serde_json::{Map, Value};

const CACHE_FIELDS: &[&str] = &[
    "cached_tokens",
    "cache_read_input_tokens",
    "cache_read_tokens",
    "prompt_cache_hit_tokens",
    "cache_write_tokens",
    "cache_creation_input_tokens",
    "created_cache_tokens",
    "cached_creation_tokens",
    "cache_creation_tokens",
    "cache_write_input_tokens",
    "cache_creation",
    "cache_write_5m_tokens",
    "cache_write_1h_tokens",
    "claude_cache_creation_5_m_tokens",
    "claude_cache_creation_1_h_tokens",
    "cached_tokens_details",
    "audio_cached_tokens",
    "cache_write_tokens_details",
];

fn merge(target: &mut Value, source: &Value) -> Result<(), &'static str> {
    if source.is_null() {
        return Ok(());
    }
    if target.is_null() {
        *target = source.clone();
        return Ok(());
    }
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            merge(target.entry(key).or_insert(Value::Null), value)?;
        }
        return Ok(());
    }
    if target == source {
        Ok(())
    } else {
        Err("conflicting_cache_counters")
    }
}

/// Normalize cache aliases across the usage root and its input detail objects.
/// Explicit zero is observed; null is absent; contradictory mirrors are errors.
pub fn compatible_cache_details(value: &Value) -> Result<PromptTokensDetails, &'static str> {
    let root = value.as_object().ok_or("invalid_usage_object")?;
    let mut details = Value::Object(Map::new());
    for key in ["prompt_tokens_details", "input_tokens_details"] {
        if let Some(source) = root.get(key).filter(|v| !v.is_null()) {
            let source = source.as_object().ok_or("invalid_input_details")?;
            for key in CACHE_FIELDS
                .iter()
                .copied()
                .chain(["audio_tokens", "image_tokens"])
            {
                if let Some(value) = source.get(key) {
                    merge(
                        details
                            .as_object_mut()
                            .unwrap()
                            .entry(key)
                            .or_insert(Value::Null),
                        value,
                    )?;
                }
            }
        }
    }
    for key in CACHE_FIELDS {
        if let Some(source) = root.get(*key) {
            merge(
                details
                    .as_object_mut()
                    .unwrap()
                    .entry(*key)
                    .or_insert(Value::Null),
                source,
            )?;
        }
    }
    serde_json::from_value(details).map_err(|_| "invalid_cache_details")
}
