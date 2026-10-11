//! Retained detail observations; raw and aggregates overlap and are selected per grain.
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
    prepared(keys, table, predicate, true)
}

/// Keys, table and predicate are validated/internal SQL; user strings stay bound.
pub(super) fn prepared(keys: &str, table: &str, predicate: &str, historical: bool) -> String {
    let details = observation_source(
        keys,
        predicate,
        &Observation {
            table: "mv_token_details_5min",
            fields: &FIELDS[..DETAIL_FIELD_COUNT],
        },
    );
    let ttl = observation_source(
        keys,
        predicate,
        &Observation {
            table: "mv_cache_ttl_5min",
            fields: &TTL_FIELDS,
        },
    );
    let units = super::input_units::prepared_with_calibration(keys, table, predicate, historical);
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
    fields: &'a [&'a str],
}

fn observation_source(keys: &str, predicate: &str, observation: &Observation<'_>) -> String {
    let observed_table = observation.table;
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let aggregate = format!(
        "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, {} FROM {observed_table} WHERE {predicate}",
        merged_sql(observation.fields)
    );
    super::measurement_coverage::aggregate_source(keys, &aggregate)
}

/// Combine two independent aggregate families without another HTTP round trip.
pub(super) fn with_provenance(keys: &str, table: &str, predicate: &str) -> String {
    let provenance = super::usage_sources::source(keys, predicate);
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
