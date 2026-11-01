//! Detail observation of complete settlement records; legacy counters stay unknown.
use super::stats::{ch_i64, scaled_ratio};
use serde_json::{Value, json};

/// (数值列, 上报标志列)：CH 事件表与 PG 账本（billing_records 的生成列）同名同义。
const FIELDS: [(&str, &str); 9] = [
    ("audio_prompt_tokens", "audio_prompt_reported"),
    ("image_prompt_tokens", "image_prompt_reported"),
    ("audio_completion_tokens", "audio_completion_reported"),
    ("image_completion_tokens", "image_completion_reported"),
    ("cache_read_audio_tokens", "cache_read_audio_reported"),
    ("cache_read_image_tokens", "cache_read_image_reported"),
    ("cache_write_audio_tokens", "cache_write_audio_reported"),
    ("cache_write_image_tokens", "cache_write_image_reported"),
    ("reasoning_tokens", "reasoning_reported"),
];

pub(super) fn ch_detail_sql() -> String {
    FIELDS.iter().map(|(value, flag)| format!(
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

/// PG 账本汇总：读 billing_records 从 usage_details 生成的列。数字只认不越界的 JSON 整数、布尔只认
/// JSON 布尔，其余写入时就成了 NULL（未上报），一条脏记录不会让整页汇总报错。
pub(super) fn pg_sql() -> String {
    let details = FIELDS.iter().map(|(value, flag)| format!(
        "'observed_{value}', SUM(b.{value}) FILTER (WHERE b.{flag}), 'observed_{value}_n', COUNT(b.{value}) FILTER (WHERE b.{flag})"
    )).collect::<Vec<_>>().join(", ");
    let known = "b.cache_write_reported AND b.cache_write_5m_tokens >= 0 AND b.cache_write_1h_tokens >= 0 \
        AND b.cache_write_5m_tokens + b.cache_write_1h_tokens = b.cache_write_tokens";
    let ttl = TTL_AXES.map(|name| format!(
        "'observed_{name}', SUM(b.{name}) FILTER (WHERE {known}), 'observed_{name}_n', COUNT(b.{name}) FILTER (WHERE {known})"
    )).join(", ");
    format!("{details}, {ttl}")
}

pub(super) fn enrich(row: &Value, result: &mut Value, records: i64) {
    let mut observations = json!({});
    for name in FIELDS.iter().map(|(name, _)| *name).chain(TTL_AXES) {
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
    if FIELDS.iter().all(|(_, flag)| row[*flag].is_null()) {
        return Value::Null;
    }
    let mut details = json!({});
    for (index, (_, flag)) in FIELDS.iter().enumerate() {
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
