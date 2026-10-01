//! Elapsed duration includes all measured requests; Token throughput is independent.
use super::stats::rate_bp;
use serde_json::{Map, Value, json};

pub(super) fn source(keys: &str, expected_table: &str, predicate: &str) -> String {
    super::performance_source::source(
        super::performance_source::Kind::Latency,
        keys,
        expected_table,
        predicate,
    )
}

pub(super) fn metrics(
    sum: i64,
    samples: i64,
    output: i64,
    requests: i64,
    observed: i64,
) -> Map<String, Value> {
    let complete = observed == requests;
    let mut result = Map::new();
    for (key, value) in [
        ("latency_sum_ms", sum),
        ("latency_samples", samples),
        ("latency_observed_requests", observed),
        ("performance_requests", observed),
        ("performance_completion_tokens", output),
    ] {
        result.insert(key.into(), json!(value));
    }
    result.insert("latency_history_complete".into(), json!(complete));
    for (key, value) in [
        ("latency_history_coverage_bp", observed),
        ("latency_sample_coverage_bp", samples),
    ] {
        result.insert(
            key.into(),
            if requests > 0 {
                json!(rate_bp(value, requests).min(10_000))
            } else {
                Value::Null
            },
        );
    }
    result.insert(
        "avg_latency_ms".into(),
        if complete && samples > 0 {
            json!(sum / samples)
        } else {
            Value::Null
        },
    );
    result
}
