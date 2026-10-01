//! Token throughput has its own paired samples; character latency is not a Token denominator.
use super::measurement_coverage::Mode;
use super::stats::{ch_i64, scaled_ratio};
use serde_json::{Map, Value, json};

pub(super) const FIELDS: [&str; 6] = [
    "output_rate_observed",
    "output_rate_sum",
    "output_rate_samples",
    "output_rate_output",
    "output_rate_known",
    "output_rate_tokens",
];
const TOKEN: &str = "input_unit = 'tokens' AND isNull(input_characters)";
const DURATION: &str = "ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1";
const MERGED: &str = "countMerge(requests) AS output_rate_observed, sumIfMerge(total_ms) AS output_rate_sum, countIfMerge(samples) AS output_rate_samples, sumIfMerge(output_tokens) AS output_rate_output, countIfMerge(known_units) AS output_rate_known, countIfMerge(token_requests) AS output_rate_tokens";

pub(super) fn raw_sql() -> String {
    let valid = format!("({TOKEN}) AND {DURATION}");
    format!(
        "count() AS output_rate_observed, sumIf(toUInt64(latency_ms), {valid}) AS output_rate_sum, countIf({valid}) AS output_rate_samples, sumIf(toUInt64(completion_tokens), {valid}) AS output_rate_output, countIf(({TOKEN}) OR ({})) AS output_rate_known, countIf({TOKEN}) AS output_rate_tokens",
        super::input_units::VALID_CHAR
    )
}

pub(super) fn pg_sql() -> String {
    let token = "b.usage_details->>'input_unit' = 'tokens' AND b.usage_details->>'input_characters' IS NULL";
    let valid = format!("({token}) AND b.status IN (20,30,40) AND b.latency_ms >= 0");
    let character = "b.usage_details->>'input_unit' = 'characters' AND b.usage_details->>'input_characters' IS NOT NULL AND b.prompt_tokens = 0 AND b.completion_tokens = 0 AND b.cached_tokens = 0 AND b.reasoning_tokens = 0";
    format!(
        "'output_rate_observed', COUNT(*), 'output_rate_sum', COALESCE(SUM(b.latency_ms) FILTER (WHERE {valid}), 0), 'output_rate_samples', COUNT(*) FILTER (WHERE {valid}), 'output_rate_output', COALESCE(SUM(b.completion_tokens) FILTER (WHERE {valid}), 0), 'output_rate_known', COUNT(*) FILTER (WHERE ({token}) OR ({character})), 'output_rate_tokens', COUNT(*) FILTER (WHERE {token})"
    )
}

pub(super) fn sum_sql() -> String {
    FIELDS
        .map(|field| format!("sum({field}) AS {field}"))
        .join(", ")
}

pub(super) fn accumulate(total: &mut Value, row: &Value) {
    for field in FIELDS {
        total[field] = json!(ch_i64(total, field).saturating_add(ch_i64(row, field)));
    }
}

pub(super) fn prepared(keys: &str, table: &str, predicate: &str, mode: Mode) -> String {
    prepared_with_calibration(keys, table, predicate, mode, true)
}

pub(super) fn prepared_with_calibration(
    keys: &str,
    table: &str,
    predicate: &str,
    mode: Mode,
    historical: bool,
) -> String {
    let base = uncalibrated(keys, table, predicate, mode);
    if !historical {
        return base;
    }
    let correction = super::input_units::correction_source(keys, predicate);
    let time = time_sql(table);
    let expected = format!(
        "(WITH {time} SELECT {keys},countMerge(requests) AS n FROM {table} WHERE {predicate} GROUP BY {keys})"
    );
    let selected_keys = keys
        .split(", ")
        .map(|key| format!("r.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let known = "r.output_rate_known+ifNull(c.legacy_character_n,0)";
    // Proven old characters can establish non-Token coverage even when their
    // rate MV was never installed. They add no Token/duration sample.
    format!(
        "(SELECT {selected_keys},greatest(r.output_rate_observed,{known})+toInt64(throwIf({known}>e.n)) AS output_rate_observed,r.output_rate_sum,r.output_rate_samples,r.output_rate_output,{known} AS output_rate_known,r.output_rate_tokens FROM {base} r LEFT JOIN {correction} c USING ({keys}) LEFT JOIN {expected} e USING ({keys}))"
    )
}

fn time_sql(table: &str) -> &'static str {
    match table {
        "mv_channel_5min" => "toStartOfHour(ts5) AS hour, toDate(ts5) AS day",
        "mv_key_model_day" | "mv_user_model_day" | "mv_user_day" | "mv_apikey_day" => {
            "toStartOfDay(day) AS hour"
        }
        _ => "toDate(hour) AS day",
    }
}

fn uncalibrated(keys: &str, table: &str, predicate: &str, mode: Mode) -> String {
    let time = time_sql(table);
    let raw_sql = raw_sql();
    let aggregate = format!(
        "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day SELECT {keys}, {MERGED} FROM mv_output_rate_5min WHERE {predicate}"
    );
    let raw = format!(
        "WITH toStartOfFiveMinutes(ts) AS ts5, toStartOfHour(ts) AS hour, toDate(ts) AS day SELECT {keys}, {raw_sql} FROM request_log_raw WHERE {predicate}"
    );
    let expected = format!(
        "WITH {time} SELECT {keys}, countMerge(requests) AS n FROM {table} WHERE {predicate} GROUP BY {keys}"
    );
    let counts = format!(
        "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day SELECT {keys}, countMerge(requests) AS n FROM mv_output_rate_5min WHERE {predicate} GROUP BY {keys}"
    );
    if mode != Mode::Recover {
        return super::measurement_coverage::fast_source(
            mode, keys, &expected, &counts, &aggregate, &raw,
        );
    }
    let selected_keys = keys
        .split(", ")
        .map(|key| format!("e.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let selected = FIELDS.map(|field| format!(
        "toInt64(if(use_aggregate, ifNull(a.{field}, 0), if(ifNull(r.output_rate_observed, 0) <= e.n, ifNull(r.{field}, 0), 0))) AS {field}"
    )).join(", ");
    format!(
        "(WITH e AS ({expected}), a AS ({aggregate} GROUP BY {keys}), \
        missing AS (SELECT {keys} FROM e LEFT JOIN a USING ({keys}) WHERE e.n != ifNull(a.output_rate_observed, 0)), \
        r AS ({raw} AND ({keys}) IN (SELECT {keys} FROM missing) GROUP BY {keys}) \
        SELECT {selected_keys}, {selected}, \
        (ifNull(a.output_rate_observed, 0) <= e.n AND ifNull(a.output_rate_observed, 0) >= if(ifNull(r.output_rate_observed, 0) <= e.n, ifNull(r.output_rate_observed, 0), 0)) AS use_aggregate \
        FROM e LEFT JOIN a USING ({keys}) LEFT JOIN r USING ({keys}))"
    )
}

pub(super) fn metrics(row: &Value, requests: i64) -> Map<String, Value> {
    let read = |field| ch_i64(row, field);
    let [observed, duration, samples, output, known, tokens] = FIELDS.map(read);
    let valid = requests >= 0
        && [observed, duration, samples, output, known, tokens]
            .iter()
            .all(|v| *v >= 0)
        && samples <= tokens
        && tokens <= known
        && known <= observed
        && observed <= requests
        && (samples > 0 || (duration == 0 && output == 0));
    let [observed, duration, samples, output, known, tokens] = if valid {
        [observed, duration, samples, output, known, tokens]
    } else {
        [0; 6]
    };
    let history_complete = requests > 0 && observed == requests;
    let unit_complete = requests > 0 && known == requests;
    let complete = history_complete && unit_complete;
    let speed = if samples > 0 && duration > 0 {
        json!(scaled_ratio(output, duration, 1_000_000))
    } else {
        Value::Null
    };
    let full = if complete { speed.clone() } else { Value::Null };
    let coverage = |count| {
        if requests > 0 {
            json!(scaled_ratio(count, requests, 10_000))
        } else {
            Value::Null
        }
    };
    json!({
        "tokens_per_1k_sec":full,"avg_output_tps_milli":full,
        "observed_output_tps_milli":speed,
        "output_tps_basis":"settled_token_output_per_measured_token_request_latency",
        "output_tps_observed_requests":observed,
        "output_tps_unit_known_requests":known,
        "output_tps_token_requests":tokens,
        "output_tps_samples":samples,
        "output_tps_duration_ms":duration,
        "output_tps_completion_tokens":output,
        "output_tps_unknown_unit_requests":requests.saturating_sub(known),
        "output_tps_history_coverage_bp":coverage(observed),
        "output_tps_unit_coverage_bp":coverage(known),
        "output_tps_sample_coverage_bp":if tokens>0 {json!(scaled_ratio(samples,tokens,10_000))} else {Value::Null},
        "output_tps_history_complete":history_complete,
        "output_tps_unit_complete":unit_complete,
        "performance_completion_tokens":output
    }).as_object().cloned().unwrap_or_default()
}

pub(super) async fn enrich_entities(
    ch: &okapi_store::ChClient,
    rows: &mut [Value],
    key: &str,
    table: &str,
    predicate: &str,
) -> Result<(), crate::gateway::error::AppError> {
    let source = prepared(key, table, predicate, Mode::Recover);
    let data = ch
        .query_json_each_row(&format!(
            "SELECT toString({key}) AS metric_key, * FROM {source}"
        ))
        .await?;
    let rates: std::collections::HashMap<_, _> = data
        .iter()
        .filter_map(|row| row["metric_key"].as_str().map(|id| (id, row)))
        .collect();
    let empty = json!({});
    for row in rows {
        let rate = rates
            .get(row[key].as_str().unwrap_or_default())
            .copied()
            .unwrap_or(&empty);
        let requests = ch_i64(row, "requests");
        if let Some(object) = row.as_object_mut() {
            object.extend(metrics(rate, requests));
        }
    }
    Ok(())
}

pub(super) async fn enrich_model_days(
    ch: &okapi_store::ChClient,
    rows: &mut [Value],
    table: &str,
    predicate: &str,
) -> Result<(), crate::gateway::error::AppError> {
    let source = prepared("day, model", table, predicate, Mode::Recover);
    let data = ch
        .query_json_each_row(&format!("SELECT * FROM {source}"))
        .await?;
    let key = |row: &Value| {
        (
            row["day"].as_str().unwrap_or_default().to_owned(),
            row["model"].as_str().unwrap_or_default().to_owned(),
        )
    };
    let rates: std::collections::HashMap<_, _> = data.iter().map(|row| (key(row), row)).collect();
    let empty = json!({});
    for row in rows {
        let rate = rates.get(&key(row)).copied().unwrap_or(&empty);
        let requests = ch_i64(row, "requests");
        if let Some(object) = row.as_object_mut() {
            object.extend(metrics(rate, requests));
        }
    }
    Ok(())
}

pub(super) async fn enrich_quality(
    ch: &okapi_store::ChClient,
    rows: &mut [Value],
    scope: super::ttft::Scope,
    since: i64,
) -> Result<(), crate::gateway::error::AppError> {
    use super::ttft::Scope;
    let (key, row_key, table, predicate, kind) = match scope {
        Scope::Model => (
            "model",
            "model",
            "mv_model_hour",
            format!("hour >= fromUnixTimestamp({since})"),
            "String",
        ),
        Scope::Channel => (
            "channel_id",
            "channel_id",
            "mv_channel_5min",
            format!("ts5 >= fromUnixTimestamp({since})"),
            "UInt32",
        ),
        Scope::Timeline(id) => (
            "ts5",
            "bucket",
            "mv_channel_5min",
            format!("ts5 >= fromUnixTimestamp({since}) AND channel_id = {id}"),
            "DateTime",
        ),
    };
    if rows.is_empty() {
        return Ok(());
    }
    let values: Vec<String> = rows
        .iter()
        .map(|row| {
            row[row_key]
                .as_str()
                .map_or_else(|| row[row_key].to_string(), str::to_owned)
        })
        .collect();
    let names: Vec<String> = (0..values.len()).map(|i| format!("rate_key_{i}")).collect();
    let bindings = names
        .iter()
        .map(|name| format!("{{{name}:{kind}}}"))
        .collect::<Vec<_>>()
        .join(", ");
    let params: Vec<(&str, &str)> = names
        .iter()
        .zip(&values)
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let source = prepared(
        key,
        table,
        &format!("{predicate} AND {key} IN ({bindings})"),
        Mode::Recover,
    );
    let data = ch
        .query_with_params(
            &format!("SELECT toString({key}) AS metric_key, * FROM {source}"),
            &params,
        )
        .await?;
    let map: std::collections::HashMap<_, _> = data
        .iter()
        .filter_map(|row| row["metric_key"].as_str().map(|key| (key, row)))
        .collect();
    let empty = json!({});
    for (row, key) in rows.iter_mut().zip(values) {
        let rate = map.get(key.as_str()).copied().unwrap_or(&empty);
        let requests = ch_i64(row, "requests");
        if let Some(object) = row.as_object_mut() {
            object.extend(metrics(rate, requests));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_pair_outputs_and_duration_preserving_zero_partial_and_integer_precision() {
        let mut row = json!({"output_rate_observed":2,"output_rate_known":2,"output_rate_tokens":1,"output_rate_samples":1,"output_rate_sum":1000,"output_rate_output":200});
        assert_eq!(metrics(&row, 2)["tokens_per_1k_sec"], 200_000);
        let partial = metrics(&row, 3);
        assert!(partial["tokens_per_1k_sec"].is_null());
        assert_eq!(partial["observed_output_tps_milli"], 200_000);
        assert_eq!(partial["output_tps_unknown_unit_requests"], 1);
        let mut unknown_row = row.clone();
        unknown_row["output_rate_observed"] = json!(3);
        let unknown = metrics(&unknown_row, 3);
        assert_eq!(unknown["output_tps_history_complete"], true);
        assert_eq!(unknown["output_tps_unit_complete"], false);
        assert!(unknown["tokens_per_1k_sec"].is_null());
        row["output_rate_output"] = json!(0);
        assert_eq!(metrics(&row, 2)["tokens_per_1k_sec"], 0);
        row["output_rate_sum"] = json!(0);
        assert!(metrics(&row, 2)["tokens_per_1k_sec"].is_null());
        row["output_rate_sum"] = json!(200_000_000_000_i64);
        row["output_rate_output"] = json!(20_000_000_000_000_i64);
        assert_eq!(metrics(&row, 2)["tokens_per_1k_sec"], 100_000_000);
        row["output_rate_samples"] = json!(2);
        assert!(metrics(&row, 2)["observed_output_tps_milli"].is_null());
    }
}
