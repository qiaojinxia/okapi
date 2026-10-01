//! Retained detail observations; raw and aggregates overlap and are selected per grain.
use super::measurement_coverage::Mode;
use super::stats::{ch_i64, scaled_ratio};
use crate::gateway::error::AppError;
use okapi_store::ChClient;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub(super) const TTL_FIELDS: [&str; 5] = [
    "ttl_observed",
    "observed_cache_write_5m_tokens",
    "observed_cache_write_5m_tokens_n",
    "observed_cache_write_1h_tokens",
    "observed_cache_write_1h_tokens_n",
];
const DETAIL_FIELD_COUNT: usize = 19;

pub(super) const FIELDS: [&str; 24] = [
    "detail_observed",
    "observed_audio_prompt_tokens",
    "observed_audio_prompt_tokens_n",
    "observed_image_prompt_tokens",
    "observed_image_prompt_tokens_n",
    "observed_audio_completion_tokens",
    "observed_audio_completion_tokens_n",
    "observed_image_completion_tokens",
    "observed_image_completion_tokens_n",
    "observed_cache_read_audio_tokens",
    "observed_cache_read_audio_tokens_n",
    "observed_cache_read_image_tokens",
    "observed_cache_read_image_tokens_n",
    "observed_cache_write_audio_tokens",
    "observed_cache_write_audio_tokens_n",
    "observed_cache_write_image_tokens",
    "observed_cache_write_image_tokens_n",
    "observed_reasoning_tokens",
    "observed_reasoning_tokens_n",
    "ttl_observed",
    "observed_cache_write_5m_tokens",
    "observed_cache_write_5m_tokens_n",
    "observed_cache_write_1h_tokens",
    "observed_cache_write_1h_tokens_n",
];

pub(super) fn sum_sql() -> String {
    FIELDS
        .into_iter()
        .chain(super::input_units::FIELDS)
        .map(|field| format!("sum({field}) AS {field}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn merged_sql(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| {
            let expression = if matches!(*field, "detail_observed" | "ttl_observed") {
                "countMerge(requests)".to_owned()
            } else if field.ends_with("_n") {
                format!("countIfMerge({field})")
            } else {
                format!("sumIfMerge({field})")
            };
            format!("{expression} AS {field}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn source(keys: &str, table: &str, predicate: &str) -> String {
    prepared(
        keys,
        table,
        predicate,
        Mode::Recover,
        Mode::Recover,
        Mode::Recover,
        true,
    )
}

/// Keys, table and predicate are validated/internal SQL; user strings stay bound.
pub(super) fn prepared(
    keys: &str,
    table: &str,
    predicate: &str,
    mode: Mode,
    units_mode: Mode,
    ttl_mode: Mode,
    historical: bool,
) -> String {
    let details = observation_source(
        keys,
        table,
        predicate,
        mode,
        &Observation {
            table: "mv_token_details_5min",
            count: "detail_observed",
            fields: &FIELDS[..DETAIL_FIELD_COUNT],
            raw: super::usage_observations::ch_detail_sql(),
        },
    );
    let ttl = observation_source(
        keys,
        table,
        predicate,
        ttl_mode,
        &Observation {
            table: "mv_cache_ttl_5min",
            count: "ttl_observed",
            fields: &TTL_FIELDS,
            raw: super::usage_observations::ch_ttl_sql(),
        },
    );
    let units = super::input_units::prepared_with_calibration(
        keys, table, predicate, units_mode, historical,
    );
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let selected = super::input_units::FIELDS
        .map(|field| format!("iu.{field} AS {field}"))
        .join(", ");
    let ttl_selected = TTL_FIELDS
        .iter()
        .map(|field| format!("ct.{field} AS {field}"))
        .collect::<Vec<_>>()
        .join(", ");
    let keys_selected = keys
        .split(", ")
        .map(|key| format!("td.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let details_selected = FIELDS[..DETAIL_FIELD_COUNT]
        .iter()
        .map(|field| format!("td.{field} AS {field}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(SELECT {keys_selected}, {details_selected}, {selected}, {ttl_selected} FROM {details} td LEFT JOIN {units} iu USING ({keys}) LEFT JOIN {ttl} ct USING ({keys}))"
    )
}

struct Observation<'a> {
    table: &'static str,
    count: &'static str,
    fields: &'a [&'a str],
    raw: String,
}

fn observation_source(
    keys: &str,
    table: &str,
    predicate: &str,
    mode: Mode,
    observation: &Observation<'_>,
) -> String {
    let observed_table = observation.table;
    let observed_count = observation.count;

    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let time = match table {
        "mv_channel_5min" => "toStartOfHour(ts5) AS hour, toDate(ts5) AS day",
        "mv_key_model_day" | "mv_user_model_day" | "mv_user_day" | "mv_apikey_day" => {
            "toStartOfDay(day) AS hour"
        }
        _ => "toDate(hour) AS day",
    };
    let aggregate = merged_sql(observation.fields);
    let raw = format!("count() AS {observed_count}, {}", observation.raw);
    if mode != Mode::Recover {
        let expected = format!(
            "WITH {time}, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS n FROM {table} WHERE {predicate} GROUP BY {keys}"
        );
        let counts = format!(
            "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS n FROM {observed_table} WHERE {predicate} GROUP BY {keys}"
        );
        let aggregate = format!(
            "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, {aggregate} FROM {observed_table} WHERE {predicate}"
        );
        let raw = format!(
            "WITH toStartOfFiveMinutes(ts) AS ts5, toStartOfHour(ts) AS hour, toDate(ts) AS day, toUInt8(1) AS source_scope SELECT {keys}, {raw} FROM request_log_calls WHERE {predicate}"
        );
        return super::measurement_coverage::fast_source(
            mode, keys, &expected, &counts, &aggregate, &raw,
        );
    }
    let selected_keys = keys
        .split(", ")
        .map(|key| format!("e.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let selected = observation.fields.iter().map(|field| format!(
        "toInt64(if(use_aggregate, ifNull(a.{field}, 0), if(ifNull(r.{observed_count}, 0) <= e.expected, ifNull(r.{field}, 0), 0))) AS {field}"
    )).collect::<Vec<_>>().join(", ");
    format!(
        "(WITH \
        e AS (SELECT {keys}, countMerge(requests) AS expected FROM (SELECT *, {time}, toUInt8(1) AS source_scope FROM {table} WHERE {predicate}) WHERE {predicate} GROUP BY {keys}), \
        a AS (SELECT {keys}, {aggregate} FROM (SELECT *, toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope FROM {observed_table}) WHERE {predicate} GROUP BY {keys}), \
        missing AS (SELECT {keys} FROM e LEFT JOIN a USING ({keys}) WHERE e.expected != ifNull(a.{observed_count}, 0)), \
        r AS (SELECT {keys}, {raw} FROM (SELECT *, toStartOfFiveMinutes(ts) AS ts5, toStartOfHour(ts) AS hour, toDate(ts) AS day, toUInt8(1) AS source_scope FROM request_log_calls) raw_rows INNER JOIN missing USING ({keys}) WHERE {predicate} GROUP BY {keys}) \
        SELECT {selected_keys}, {selected}, \
        (ifNull(a.{observed_count}, 0) <= e.expected AND ifNull(a.{observed_count}, 0) >= if(ifNull(r.{observed_count}, 0) <= e.expected, ifNull(r.{observed_count}, 0), 0)) AS use_aggregate \
        FROM e LEFT JOIN a USING ({keys}) LEFT JOIN r USING ({keys}))"
    )
}

/// Combine two independent aggregate families without another HTTP round trip.
pub(super) fn with_provenance(keys: &str, table: &str, predicate: &str) -> String {
    let provenance = super::usage_sources::source(keys, table, predicate);
    let details = source(keys, table, predicate);
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let selected = FIELDS
        .into_iter()
        .chain(super::input_units::FIELDS)
        .map(|field| format!("td.{field} AS {field}"))
        .collect::<Vec<_>>()
        .join(", ");
    let source_fields = super::usage_sources::FIELDS
        .map(|field| {
            if field == "source_prompt_total" {
                format!("toInt64(us.{field})-if(us.source_observed=us.source_expected,ifNull(td.legacy_characters,0),toInt64(0)) AS {field}")
            } else {
                format!("us.{field} AS {field}")
            }
        }).join(", ");
    let keys_selected = keys
        .split(", ")
        .map(|key| format!("us.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(SELECT {keys_selected}, {source_fields}, {selected} FROM {provenance} us LEFT JOIN {details} td USING ({keys}))"
    )
}

pub(super) fn metrics(row: &Value, records: i64) -> Map<String, Value> {
    let mut result = json!({});
    super::usage_observations::enrich(row, &mut result, records);
    let observed = ch_i64(row, "detail_observed");
    result["token_detail_history"] = json!({
        "observed_requests": observed,
        "coverage_bp": if records > 0 { json!(scaled_ratio(observed, records, 10_000)) } else { Value::Null },
        "complete": records > 0 && observed == records,
    });
    let ttl_observed = ch_i64(row, "ttl_observed");
    result["cache_write_ttl_history"] = json!({
        "observed_requests": ttl_observed,
        "coverage_bp": if records > 0 { json!(scaled_ratio(ttl_observed,records,10000)) } else { Value::Null },
        "complete": records > 0 && ttl_observed == records,
    });
    let mut result = result.as_object().cloned().unwrap_or_default();
    result.extend(super::input_units::metrics(row, records));
    result
}

pub(super) fn accumulate(total: &mut Value, row: &Value) {
    for field in FIELDS.into_iter().chain(super::input_units::FIELDS) {
        total[field] = json!(ch_i64(total, field).saturating_add(ch_i64(row, field)));
    }
}

pub(super) async fn enrich_trend(
    ch: &ChClient,
    bucket_expression: &str,
    predicate: &str,
    rows: &mut [Value],
) -> Result<(), AppError> {
    if rows.is_empty() {
        return Ok(());
    }
    let source = source("hour, model", "mv_model_hour", predicate);
    let sums = sum_sql();
    let sql = format!(
        "SELECT {bucket_expression} AS bucket, model, {sums} FROM {source} GROUP BY bucket, model"
    );
    let key = |row: &Value| (row["bucket"].to_string(), row["model"].to_string());
    let measured: HashMap<_, _> = ch
        .query_json_each_row(&sql)
        .await?
        .into_iter()
        .map(|row| (key(&row), row))
        .collect();
    for row in rows {
        if let Some(details) = measured.get(&key(row)) {
            for field in FIELDS.into_iter().chain(super::input_units::FIELDS) {
                row[field] = details[field].clone();
            }
        }
    }
    Ok(())
}
