//! Additive TTFT statistics with the same valid-sample rule as percentiles.
//! Use one complete source per grain. Raw and the new MV overlap, so never add them.
use super::stats::rate_bp;
use serde_json::{Map, Value, json};

pub(super) const VALID: &str = "stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1";

pub(super) fn source(keys: &str, expected_table: &str, predicate: &str) -> String {
    super::performance_source::source(
        super::performance_source::Kind::Ttft,
        keys,
        expected_table,
        predicate,
    )
}

pub(super) fn metrics(sum: i64, samples: i64, requests: i64, observed: i64) -> Map<String, Value> {
    let complete = requests == observed;
    let mut result = Map::new();
    result.insert("ttft_sum_ms".into(), json!(sum));
    result.insert("ttft_samples".into(), json!(samples));
    result.insert("ttft_observed_requests".into(), json!(observed));
    result.insert("ttft_history_complete".into(), json!(complete));
    result.insert(
        "ttft_history_coverage_bp".into(),
        if requests > 0 {
            json!(rate_bp(observed, requests).min(10_000))
        } else {
            Value::Null
        },
    );
    result.insert(
        "avg_ttft_ms".into(),
        if complete && samples > 0 {
            json!(sum / samples)
        } else {
            Value::Null
        },
    );
    result
}
