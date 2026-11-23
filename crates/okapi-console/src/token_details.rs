//! Retained detail observations; raw and aggregates overlap and are selected per grain.
use super::stats::scaled_ratio;
use crate::ch_access::ch_i64;
use okapi_kernel::error::AppError;
use okapi_store::ChClient;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

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

pub(super) fn merged_sql(fields: &[&str]) -> String {
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
/// Single-table read from unified `mv_analysis_5min` (no 3-way MV join).
pub(super) fn prepared(keys: &str, table: &str, predicate: &str, historical: bool) -> String {
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let details_and_units = format!("{}, {}", merged_sql(&FIELDS), super::input_units::MERGED);
    let aggregate = format!(
        "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope \
         SELECT {keys}, {details_and_units} FROM mv_analysis_5min WHERE {predicate}"
    );
    let base = super::measurement_coverage::aggregate_source(keys, &aggregate);
    if !historical {
        return format!(
            "(SELECT u.*, toInt64(0) AS legacy_characters, toInt64(0) AS legacy_character_n FROM {} u)",
            super::analysis_source::as_view(&base),
        );
    }
    calibrate_units(keys, table, predicate, &base, &FIELDS, &[])
}

fn calibrate_units(
    keys: &str,
    table: &str,
    predicate: &str,
    base: &str,
    passthrough: &[&str],
    source_fields: &[&str],
) -> String {
    let legacy = super::input_units::correction_source(keys, predicate);
    let time = super::observation_sources::Grain::for_table(table).time_sql();
    let expected = format!(
        "(WITH {time}, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS expected FROM {table} WHERE {predicate} GROUP BY {keys})"
    );
    let keys_selected = keys
        .split(", ")
        .map(|key| format!("u.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let count = "ifNull(c.legacy_character_n, 0)";
    let quantity = "ifNull(c.legacy_characters, 0)";
    let mut cols: Vec<String> = Vec::new();
    for field in source_fields {
        if *field == "source_prompt_total" {
            cols.push(format!("toInt64(u.{field}) - {quantity} AS {field}"));
        } else {
            cols.push(format!("u.{field} AS {field}"));
        }
    }
    for field in passthrough {
        cols.push(format!("u.{field} AS {field}"));
    }
    let guard = format!(
        "toInt64(throwIf({count} + u.unit_character_n + u.unit_token_n > greatest(e.expected, u.unit_observed)))"
    );
    cols.push(format!(
        "greatest(u.unit_observed, u.unit_character_n + u.unit_token_n + {count}) + {guard} AS unit_observed"
    ));
    cols.push(format!("u.unit_characters + {quantity} AS unit_characters"));
    cols.push(format!("u.unit_character_n + {count} AS unit_character_n"));
    cols.push("u.unit_token_n AS unit_token_n".to_owned());
    cols.push(format!("{quantity} AS legacy_characters"));
    cols.push(format!("{count} AS legacy_character_n"));
    format!(
        "(SELECT {keys_selected}, {} FROM {} u LEFT JOIN {} c ON {} LEFT JOIN {} e ON {})",
        cols.join(", "),
        super::analysis_source::as_view(base),
        super::analysis_source::as_view(&legacy),
        super::analysis_source::join_on("u", "c", keys),
        super::analysis_source::as_view(&expected),
        super::analysis_source::join_on("u", "e", keys),
    )
}

/// Combine provenance, token details, TTL and input units in one single-MV scan.
pub(super) fn with_provenance(keys: &str, table: &str, predicate: &str) -> String {
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let all_merged = format!(
        "{}, {}, {}",
        super::usage_sources::merged_sql(),
        merged_sql(&FIELDS),
        super::input_units::MERGED,
    );
    let aggregate = format!(
        "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope \
         SELECT {keys}, {all_merged} FROM mv_analysis_5min WHERE {predicate}"
    );
    let base = super::measurement_coverage::aggregate_source(keys, &aggregate);
    calibrate_units(
        keys,
        table,
        predicate,
        &base,
        &FIELDS,
        &super::usage_sources::FIELDS,
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
