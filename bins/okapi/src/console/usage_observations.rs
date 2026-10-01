//! Detail observation of complete settlement records; legacy counters stay unknown.
use super::stats::{ch_i64, scaled_ratio};
use serde_json::{Value, json};

const FIELDS: [(&str, &str, &str, &str); 9] = [
    (
        "audio_prompt_tokens",
        "audio_prompt_reported",
        "(b.usage_details->'tokens'->>'audio_prompt_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'prompt'->>'audio'",
    ),
    (
        "image_prompt_tokens",
        "image_prompt_reported",
        "(b.usage_details->'tokens'->>'image_prompt_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'prompt'->>'image'",
    ),
    (
        "audio_completion_tokens",
        "audio_completion_reported",
        "(b.usage_details->'tokens'->>'audio_completion_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'completion'->>'audio'",
    ),
    (
        "image_completion_tokens",
        "image_completion_reported",
        "(b.usage_details->'tokens'->>'image_completion_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'completion'->>'image'",
    ),
    (
        "cache_read_audio_tokens",
        "cache_read_audio_reported",
        "(b.usage_details->'tokens'->'cache_read_modalities'->>'audio_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'cache_read'->>'audio'",
    ),
    (
        "cache_read_image_tokens",
        "cache_read_image_reported",
        "(b.usage_details->'tokens'->'cache_read_modalities'->>'image_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'cache_read'->>'image'",
    ),
    (
        "cache_write_audio_tokens",
        "cache_write_audio_reported",
        "(b.usage_details->'tokens'->'cache_write_modalities'->>'audio_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'cache_write'->>'audio'",
    ),
    (
        "cache_write_image_tokens",
        "cache_write_image_reported",
        "(b.usage_details->'tokens'->'cache_write_modalities'->>'image_tokens')::bigint",
        "b.usage_details->'tokens'->'reported_details'->'cache_write'->>'image'",
    ),
    (
        "reasoning_tokens",
        "reasoning_reported",
        "b.reasoning_tokens",
        "b.usage_details->'tokens'->'reported_details'->>'reasoning'",
    ),
];

pub(super) fn ch_detail_sql() -> String {
    FIELDS.iter().map(|(value, flag, _, _)| format!(
        "sumOrNullIf({value}, ifNull({flag}, 0) = 1) AS observed_{value}, countIf(ifNull({flag}, 0) = 1 AND isNotNull({value})) AS observed_{value}_n"
    )).collect::<Vec<_>>().join(", ")
}

pub(super) const TTL_AXES: [&str; 2] = ["cache_write_5m_tokens", "cache_write_1h_tokens"];
const TTL_KNOWN: &str = "ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))";

pub(super) fn ch_ttl_sql() -> String {
    TTL_AXES.map(|name| format!(
        "sumOrNullIf({name}, {TTL_KNOWN}) AS observed_{name}, countIf({TTL_KNOWN}) AS observed_{name}_n"
    )).join(", ")
}

pub(super) fn ch_sql() -> String {
    format!("{}, {}", ch_detail_sql(), ch_ttl_sql())
}

pub(super) fn pg_sql() -> String {
    let details = FIELDS.iter().map(|(name, _, value, flag)| format!(
        "'observed_{name}', SUM({value}) FILTER (WHERE ({flag})::boolean = true), 'observed_{name}_n', COUNT({value}) FILTER (WHERE ({flag})::boolean = true)"
    )).collect::<Vec<_>>().join(", ");
    let value = |name| format!("(b.usage_details->'tokens'->>'{name}')::bigint");
    let known = format!(
        "COALESCE((b.usage_details->'tokens'->>'cache_write_reported')::boolean, false) AND {} >= 0 AND {} >= 0 AND {} + {} = {}",
        value("cache_write_5m_tokens"),
        value("cache_write_1h_tokens"),
        value("cache_write_5m_tokens"),
        value("cache_write_1h_tokens"),
        value("cache_write_tokens")
    );
    let ttl = TTL_AXES.map(|name| format!(
        "'observed_{name}', SUM({}) FILTER (WHERE {known}), 'observed_{name}_n', COUNT({}) FILTER (WHERE {known})",value(name),value(name)
    )).join(", ");
    format!("{details}, {ttl}")
}

pub(super) fn enrich(row: &Value, result: &mut Value, records: i64) {
    let mut observations = json!({});
    for name in FIELDS.iter().map(|(name, _, _, _)| *name).chain(TTL_AXES) {
        let count = ch_i64(row, &format!("observed_{name}_n"));
        let sum = &row[format!("observed_{name}")];
        let complete = records > 0 && count == records && !sum.is_null();
        let tokens = if count > 0 && !sum.is_null() {
            json!(ch_i64(row, &format!("observed_{name}")))
        } else {
            Value::Null
        };
        observations[name] = json!({
            "tokens": if complete { tokens.clone() } else { Value::Null },
            "observed_tokens": tokens, "observed_records": count,
            "coverage_bp": if records > 0 { json!(scaled_ratio(count, records, 10_000)) } else { Value::Null },
            "complete": complete,
        });
    }
    result["token_detail_observations"] = observations;
    result["token_detail_basis"] = json!("settled");
    result["token_detail_samples_basis"] = json!("stored_values");
}

pub(super) fn from_ch(row: &Value) -> Value {
    if FIELDS.iter().all(|(_, flag, _, _)| row[*flag].is_null()) {
        return Value::Null;
    }
    let mut details = json!({});
    for (index, (_, flag, _, _)) in FIELDS.iter().enumerate() {
        let value = if row[*flag].is_null() {
            Value::Null
        } else {
            json!(ch_i64(row, flag) == 1)
        };
        let (group, axis) = match index {
            0 => ("prompt", "audio"),
            1 => ("prompt", "image"),
            2 => ("completion", "audio"),
            3 => ("completion", "image"),
            4 => ("cache_read", "audio"),
            5 => ("cache_read", "image"),
            6 => ("cache_write", "audio"),
            7 => ("cache_write", "image"),
            _ => {
                details["reasoning"] = value;
                continue;
            }
        };
        details[group][axis] = value;
    }
    details
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_zero_unknown_history_partial_coverage_and_empty_ranges_are_distinct() {
        for (records, count, sum, expected, coverage) in [
            (3, 2, json!(9), Value::Null, json!(6666)),
            (2, 2, json!(0), json!(0), json!(10000)),
            (2, 0, Value::Null, Value::Null, json!(0)),
            (0, 0, Value::Null, Value::Null, Value::Null),
        ] {
            let row = json!({"observed_image_completion_tokens":sum,"observed_image_completion_tokens_n":count});
            let mut result = json!({});
            enrich(&row, &mut result, records);
            let field = &result["token_detail_observations"]["image_completion_tokens"];
            assert_eq!(field["tokens"], expected);
            assert_eq!(field["coverage_bp"], coverage);
            assert_eq!(field["observed_records"], count);
        }
        assert!(from_ch(&json!({})).is_null());
        assert_eq!(
            from_ch(&json!({"reasoning_reported":0}))["reasoning"],
            false
        );
        assert_eq!(from_ch(&json!({"reasoning_reported":1}))["reasoning"], true);
    }
}
