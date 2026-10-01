//! Valid-sample duration and TTFT percentiles with conservative, read-only legacy recovery.
//! Never merge raw with the new MV: they overlap. Counts must match the original
//! request aggregate before percentiles can represent the entire selected period.

use super::stats::{ch_i64, rate_bp};
use crate::gateway::error::AppError;
use okapi_store::ChClient;
use serde_json::{Value, json};
use std::collections::HashMap;

use super::performance_source::Kind;
const QUANTILES: [&str; 3] = ["p50_ms", "p95_ms", "p99_ms"];

#[derive(Clone, Copy)]
pub(super) enum Scope {
    Model,
    Channel,
    Timeline(i64),
}

impl Scope {
    fn row_key(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Channel => "channel_id",
            Self::Timeline(_) => "bucket",
        }
    }

    fn key(self, raw: bool) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Channel => "channel_id",
            Self::Timeline(_) if raw => "toStartOfFiveMinutes(ts)",
            Self::Timeline(_) => "ts5",
        }
    }

    fn time(self, raw: bool) -> &'static str {
        match (self, raw) {
            (Self::Model, true) => "toStartOfHour(ts)",
            (Self::Model, false) => "hour",
            (_, true) => "toStartOfFiveMinutes(ts)",
            (_, false) => "ts5",
        }
    }

    fn table(self, raw: bool, kind: Kind) -> &'static str {
        match (self, raw, kind) {
            (_, true, _) => "request_log_calls",
            (Self::Model, false, Kind::Ttft) => "mv_model_ttft_hour",
            (_, false, Kind::Ttft) => "mv_channel_ttft_5min",
            (Self::Model, false, Kind::Latency) => "mv_model_latency_hour",
            (_, false, Kind::Latency) => "mv_channel_latency_5min",
        }
    }

    fn key_type(self) -> &'static str {
        match self {
            Self::Model => "String",
            Self::Channel => "UInt32",
            Self::Timeline(_) => "DateTime",
        }
    }
}

fn row_key(row: &Value, key: &str) -> String {
    row[key]
        .as_str()
        .map_or_else(|| row[key].to_string(), str::to_owned)
}

async fn query(
    ch: &ChClient,
    scope: Scope,
    since: i64,
    keys: &[String],
    raw: bool,
    kind: Kind,
) -> Result<HashMap<String, Value>, AppError> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let names: Vec<String> = (0..keys.len()).map(|n| format!("key_{n}")).collect();
    let params: Vec<(&str, &str)> = names
        .iter()
        .zip(keys)
        .map(|(name, key)| (name.as_str(), key.as_str()))
        .collect();
    let bindings = names
        .iter()
        .map(|name| format!("{{{name}:{}}}", scope.key_type()))
        .collect::<Vec<_>>()
        .join(", ");
    let prefix = kind.prefix();
    let valid = kind.valid();
    let aggregates = if raw {
        format!(
            "count() AS observed_requests, countIf({valid}) AS valid_samples, \
             quantilesIf(0.5, 0.95, 0.99)({prefix}_ms, {valid}) AS q"
        )
    } else {
        format!(
            "countMerge(requests) AS observed_requests, countIfMerge(samples) AS valid_samples, \
         quantilesIfMerge(0.5, 0.95, 0.99)({prefix}_q) AS q"
        )
    };
    let aggregates = match (kind, raw) {
        (Kind::Latency, true) => format!(
            "{aggregates}, sumIf(toUInt64(latency_ms), {valid}) AS total_ms, sumIf(toUInt64(completion_tokens), {valid}) AS output_tokens"
        ),
        (Kind::Latency, false) => format!(
            "{aggregates}, sumIfMerge(total_ms) AS total_ms, sumIfMerge(output_tokens) AS output_tokens"
        ),
        (Kind::Ttft, _) => aggregates,
    };
    // An empty conditional aggregate is NaN. Guard the cast as well as the result:
    // correctness must not depend on the server's short_circuit_function_evaluation.
    let percentiles = QUANTILES
        .iter()
        .enumerate()
        .map(|(n, field)| {
            let index = n + 1;
            format!(
                "if(valid_samples > 0, \
                 toUInt32(if(isFinite(q[{index}]), q[{index}], 0)), NULL) AS {prefix}_{field}"
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let channel_filter = match scope {
        Scope::Timeline(id) => format!(" AND channel_id = {id}"),
        Scope::Model | Scope::Channel => String::new(),
    };
    let key = scope.key(raw);
    let sql = format!(
        "SELECT toString({key}) AS metric_key, {aggregates}, {percentiles} \
         FROM {} WHERE {} >= fromUnixTimestamp({since}) \
         AND {key} IN ({bindings}){channel_filter} GROUP BY {key}",
        scope.table(raw, kind),
        scope.time(raw),
    );
    Ok(ch
        .query_with_params(&sql, &params)
        .await?
        .into_iter()
        .map(|row| (row_key(&row, "metric_key"), row))
        .collect())
}

/// Rows already contain the authoritative request count from the original MV.
/// Bound legacy scans to the requested time range and only the displayed keys.
async fn enrich_kind(
    ch: &ChClient,
    rows: &mut [Value],
    scope: Scope,
    since: i64,
    kind: Kind,
) -> Result<(), AppError> {
    let keys: Vec<String> = rows.iter().map(|r| row_key(r, scope.row_key())).collect();
    let aggregated = query(ch, scope, since, &keys, false, kind).await?;
    let incomplete: Vec<String> = rows
        .iter()
        .zip(&keys)
        .filter(|(row, key)| {
            aggregated.get(*key).map(|r| ch_i64(r, "observed_requests"))
                != Some(ch_i64(row, "requests"))
        })
        .map(|(_, key)| key.clone())
        .collect();
    let raw = query(ch, scope, since, &incomplete, true, kind).await?;
    for (row, key) in rows.iter_mut().zip(keys) {
        let requests = ch_i64(row, "requests");
        let aggregate = aggregated.get(&key);
        let recovered = raw.get(&key);
        let (selected, source) =
            if aggregate.is_some_and(|r| ch_i64(r, "observed_requests") == requests) {
                (aggregate, "aggregate")
            } else if recovered.is_some_and(|r| ch_i64(r, "observed_requests") == requests) {
                (recovered, "raw")
            } else {
                // No union of overlapping data. Report only the better observed subset;
                // neither subset may be advertised as the whole period's percentiles.
                (
                    [aggregate, recovered]
                        .into_iter()
                        .flatten()
                        .filter(|r| ch_i64(r, "observed_requests") <= requests)
                        .max_by_key(|r| ch_i64(r, "observed_requests")),
                    "incomplete",
                )
            };
        let complete = source != "incomplete";
        let observed = selected.map_or(0, |r| ch_i64(r, "observed_requests"));
        let prefix = kind.prefix();
        row[format!("{prefix}_samples")] =
            json!(selected.map_or(0, |r| ch_i64(r, "valid_samples")));
        row[format!("{prefix}_observed_requests")] = json!(observed);
        row[format!("{prefix}_history_coverage_bp")] =
            json!(rate_bp(observed, requests).min(10_000));
        row[format!("{prefix}_history_complete")] = json!(complete);
        row[format!("{prefix}_source")] = json!(source);
        if let Kind::Latency = kind {
            let read = |field| selected.map_or(0, |r| ch_i64(r, field));
            for (key, value) in super::latency::metrics(
                read("total_ms"),
                read("valid_samples"),
                read("output_tokens"),
                requests,
                observed,
            ) {
                row[key] = value;
            }
        }
        for suffix in QUANTILES {
            let field = format!("{prefix}_{suffix}");
            row[&field] = if complete {
                selected.map_or(Value::Null, |r| r[&field].clone())
            } else {
                Value::Null
            };
        }
    }
    Ok(())
}

pub(super) async fn enrich(
    ch: &ChClient,
    rows: &mut [Value],
    scope: Scope,
    since: i64,
) -> Result<(), AppError> {
    for kind in [Kind::Ttft, Kind::Latency] {
        enrich_kind(ch, rows, scope, since, kind).await?;
    }
    super::output_rate::enrich_quality(ch, rows, scope, since).await?;
    Ok(())
}
