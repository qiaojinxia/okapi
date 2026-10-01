//! Read-only interpretation of the old gateway's speech character contract.
//! Never infer a unit from the model name, monetary amount, or current price book.
use serde_json::Value;

pub const BASIS: &str = "legacy_speech_contract_v1";
pub const ZERO_AXES: [&str; 16] = [
    "completion_tokens",
    "cached_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
    "audio_prompt_tokens",
    "audio_completion_tokens",
    "image_prompt_tokens",
    "image_completion_tokens",
    "cache_write_5m_tokens",
    "cache_write_1h_tokens",
    "cache_read_audio_tokens",
    "cache_read_image_tokens",
    "cache_write_audio_tokens",
    "cache_write_image_tokens",
    "cache_read_text_tokens",
    "cache_write_text_tokens",
];

fn field<'a>(row: &'a Value, name: &str) -> Option<&'a Value> {
    row.get(name)
        .or_else(|| row.get("usage").and_then(|usage| usage.get(name)))
}

fn integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn absent(value: Option<&Value>) -> bool {
    value.is_none_or(Value::is_null)
}

fn unknown(value: Option<&Value>) -> bool {
    absent(value)
        || value
            .and_then(Value::as_str)
            .is_some_and(|v| matches!(v, "" | "unknown"))
}

fn unreported(value: &Value) -> bool {
    value.is_null()
        || value.as_bool() == Some(false)
        || value.as_u64() == Some(0)
        || value
            .as_object()
            .is_some_and(|fields| fields.values().all(unreported))
}

fn nonnegative_decimal(value: Option<&Value>) -> bool {
    let Some(number) = value.and_then(Value::as_number) else {
        return false;
    };
    let literal = number.to_string();
    let (whole, fraction) = literal.split_once('.').unwrap_or((&literal, ""));
    !whole.is_empty()
        && whole.bytes().all(|c| c.is_ascii_digit())
        && fraction.len() <= 6
        && fraction.bytes().all(|c| c.is_ascii_digit())
}

fn snapshot(row: &Value) -> Option<Value> {
    let value = row
        .get("pricing_snapshot")
        .or_else(|| row.get("ratio_snapshot"))?;
    if let Some(json) = value.as_str() {
        serde_json::from_str(json).ok()
    } else if value.is_object() {
        Some(value.clone())
    } else {
        None
    }
}

fn old_snapshot(snapshot: &Value, row: &Value) -> bool {
    let Some(epoch) = snapshot.get("epoch").and_then(Value::as_i64) else {
        return false;
    };
    let group = row
        .get("group")
        .or_else(|| row.get("group_code"))
        .and_then(Value::as_str);
    if epoch < 0
        || row.get("pricing_epoch").and_then(integer) != Some(epoch)
        || group.is_none_or(str::is_empty)
        || snapshot.get("group").and_then(Value::as_str) != group
        || snapshot.get("input_unit").is_some()
        || snapshot.get("input_characters").is_some()
        || !absent(snapshot.get("media_units"))
        || !nonnegative_decimal(snapshot.get("group_ratio"))
        || !nonnegative_decimal(snapshot.get("user_multiplier"))
    {
        return false;
    }
    let Some(rules) = snapshot.get("rules").and_then(Value::as_array) else {
        return false;
    };
    if !rules.iter().all(|rule| {
        rule.get("code")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
            && rule
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|v| matches!(v, "volume" | "time_based" | "discount" | "surge"))
            && nonnegative_decimal(rule.get("multiplier"))
    }) {
        return false;
    }
    match snapshot.get("mode").and_then(Value::as_str) {
        Some("ratio" | "tiered") => [
            "model_ratio",
            "completion_ratio",
            "cache_ratio",
            "final_unit_price_input_per_1m_usd",
        ]
        .into_iter()
        .all(|name| nonnegative_decimal(snapshot.get(name))),
        Some("per_call") => nonnegative_decimal(snapshot.get("per_call_price_usd")),
        _ => false,
    }
}

/// Supports outbox/CH rows and the public PG log projection. Missing or conflicting
/// evidence returns None; Some(0) is an explicitly identifiable empty speech input.
#[must_use]
pub fn characters(row: &Value) -> Option<u32> {
    if row.get("endpoint").and_then(Value::as_str) != Some("/v1/audio/speech")
        || row.get("log_type").and_then(integer) != Some(2)
        || !absent(field(row, "input_characters"))
        || field(row, "input_unit").is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        || row
            .get("is_stream")
            .or_else(|| row.get("stream"))
            .is_none_or(|v| v.as_bool() != Some(false) && v.as_u64() != Some(0))
        || !unknown(field(row, "prompt_source"))
        || !unknown(field(row, "completion_source"))
        || !absent(field(row, "upstream_usage"))
        || !absent(field(row, "upstream_prompt_tokens"))
        || !absent(field(row, "upstream_completion_tokens"))
        || !absent(field(row, "cache_read_modalities"))
        || !absent(field(row, "cache_write_modalities"))
        || field(row, "reported_details").is_some_and(|value| !unreported(value))
        || field(row, "media_units").is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        || row
            .get("is_error")
            .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false) && v.as_u64() != Some(0))
        || row
            .get("status")
            .is_some_and(|v| !matches!(v.as_u64(), Some(20 | 30)))
        || conflicting_fields(row)
        || [
            "cache_read_reported",
            "cache_write_reported",
            "audio_prompt_reported",
            "image_prompt_reported",
            "audio_completion_reported",
            "image_completion_reported",
            "cache_read_audio_reported",
            "cache_read_image_reported",
            "cache_write_audio_reported",
            "cache_write_image_reported",
            "reasoning_reported",
        ]
        .into_iter()
        .any(|name| {
            field(row, name).is_some_and(|v| {
                !v.is_null() && v.as_bool() != Some(false) && v.as_u64() != Some(0)
            })
        })
        || ZERO_AXES
            .into_iter()
            .any(|name| field(row, name).is_some_and(|v| !v.is_null() && v.as_u64() != Some(0)))
    {
        return None;
    }
    let quantity = u32::try_from(field(row, "prompt_tokens")?.as_u64()?).ok()?;
    old_snapshot(&snapshot(row)?, row).then_some(quantity)
}

fn conflicting_fields(row: &Value) -> bool {
    ZERO_AXES
        .into_iter()
        .chain([
            "prompt_tokens",
            "input_unit",
            "input_characters",
            "prompt_source",
            "completion_source",
            "upstream_usage",
            "reported_details",
            "cache_read_modalities",
            "cache_write_modalities",
        ])
        .any(|name| {
            row.get(name)
                .zip(row.get("usage").and_then(|usage| usage.get(name)))
                .is_some_and(|(a, b)| !a.is_null() && !b.is_null() && a != b)
        })
        || row
            .get("is_stream")
            .zip(row.get("stream"))
            .is_some_and(|(a, b)| a.as_bool() == Some(false) && b.as_u64() != Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> Value {
        json!({"endpoint":"/v1/audio/speech","log_type":2,"is_stream":false,
            "group":"default","pricing_epoch":41,"prompt_tokens":11,
            "pricing_snapshot":{"epoch":41,"mode":"ratio","model_ratio":1,
                "completion_ratio":1,"cache_ratio":1,"group":"default",
                "group_ratio":1,"user_multiplier":1,"rules":[],
                "final_unit_price_input_per_1m_usd":2}})
    }

    #[test]
    fn accepts_old_ratio_tiered_per_call_and_exact_zero_without_price_inference() {
        let mut value = row();
        for quantity in [0, 11, u32::MAX] {
            value["prompt_tokens"] = json!(quantity);
            assert_eq!(characters(&value), Some(quantity));
        }
        value["pricing_snapshot"]["mode"] = json!("tiered");
        assert_eq!(characters(&value), Some(u32::MAX));
        value["pricing_snapshot"]["mode"] = json!("per_call");
        value["pricing_snapshot"]["per_call_price_usd"] = serde_json::from_str("0.005").unwrap();
        value["pricing_snapshot"]["user_multiplier"] = serde_json::from_str("0.5").unwrap();
        assert_eq!(characters(&value), Some(u32::MAX));
        value["model"] = json!("unrelated-'name\\");
        value["amount_micro"] = json!(0);
        assert_eq!(characters(&value), Some(u32::MAX));
    }

    #[test]
    fn contradictory_or_missing_evidence_never_becomes_a_character_measurement() {
        for (path, bad) in [
            ("/endpoint", json!("/v1/audio/transcriptions")),
            ("/log_type", json!(5)),
            ("/is_stream", json!(true)),
            ("/input_unit", json!("tokens")),
            ("/input_unit", json!("characters")),
            ("/input_characters", json!(11)),
            ("/prompt_source", json!("upstream")),
            ("/completion_source", json!("estimated")),
            ("/upstream_usage", json!({})),
            ("/upstream_prompt_tokens", json!(11)),
            ("/cache_read_modalities", json!({"audio_tokens":0})),
            ("/cache_write_modalities", json!({"text_tokens":1})),
            ("/reported_details", json!({"prompt":{"audio":true}})),
            ("/reported_details", json!({"reasoning":"false"})),
            ("/media_units", json!("{}")),
            ("/is_error", json!(1)),
            ("/status", json!(40)),
            ("/status", json!(10)),
            ("/stream", json!(1)),
            ("/usage", json!({"prompt_tokens":12})),
            ("/prompt_tokens", json!(-1)),
            ("/prompt_tokens", json!(u64::from(u32::MAX) + 1)),
            ("/pricing_epoch", json!(42)),
            ("/pricing_snapshot", Value::Null),
            ("/pricing_snapshot/group", json!("other")),
            ("/pricing_snapshot/epoch", json!(-1)),
            ("/pricing_snapshot/rules", json!({})),
            ("/pricing_snapshot/model_ratio", json!("1")),
            ("/pricing_snapshot/input_unit", json!("tokens")),
            ("/pricing_snapshot/input_characters", json!(11)),
        ] {
            let mut value = row();
            if let Some(name) = path.strip_prefix("/pricing_snapshot/") {
                value["pricing_snapshot"][name] = bad;
            } else {
                value[path.trim_start_matches('/')] = bad;
            }
            assert_eq!(characters(&value), None, "{path}: {value}");
        }
        for axis in ZERO_AXES {
            let mut value = row();
            value[axis] = json!(1);
            assert_eq!(characters(&value), None, "{axis}");
        }
    }

    #[test]
    fn public_usage_projection_preserves_nested_reporting_evidence() {
        let mut value = row();
        value["usage"] = json!({"prompt_tokens":11,"reported_details":{
            "prompt":{"audio":false,"image":false},"reasoning":false}});
        value.as_object_mut().unwrap().remove("prompt_tokens");
        assert_eq!(characters(&value), Some(11));
        value["usage"]["reported_details"]["prompt"]["image"] = json!(true);
        assert_eq!(characters(&value), None);
    }
}
