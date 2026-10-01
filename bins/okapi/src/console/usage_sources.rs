//! Token provenance is additive; raw and the new aggregate overlap and must not be added.
use super::stats::{ch_i64, rate_bp};
use crate::gateway::error::AppError;
use okapi_store::ChClient;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

pub(super) const FIELDS: [&str; 20] = [
    "source_observed",
    "source_prompt_total",
    "source_completion_total",
    "source_cached_total",
    "source_read_n",
    "source_prompt_upstream_n",
    "source_prompt_upstream_tokens",
    "source_prompt_estimated_n",
    "source_prompt_estimated_tokens",
    "source_prompt_local_override_n",
    "source_prompt_local_override_tokens",
    "source_completion_upstream_n",
    "source_completion_upstream_tokens",
    "source_completion_estimated_n",
    "source_completion_estimated_tokens",
    "source_completion_local_override_n",
    "source_completion_local_override_tokens",
    "source_cache_n",
    "source_cache_prompt",
    "source_cache_read",
];
const STATES: [&str; 3] = ["upstream", "estimated", "local_override"];

fn measured(axis: &str, state: &str) -> String {
    let original = format!("upstream_{axis}_tokens");
    let condition = match state {
        "upstream" => format!("ifNull({original} = {axis}_tokens, 0)"),
        "estimated" => format!("isNull({original})"),
        _ => format!("ifNull({original} != {axis}_tokens, 0)"),
    };
    format!("{axis}_source = '{state}' AND {condition}")
}

/// Internal identifiers only. Used by raw log summaries as well as legacy recovery.
pub(super) fn raw_sql() -> String {
    let mut expressions = vec![
        "count() AS source_observed".to_owned(),
        "sum(toUInt64(prompt_tokens)) AS source_prompt_total".to_owned(),
        "sum(toUInt64(completion_tokens)) AS source_completion_total".to_owned(),
        "sum(toUInt64(cached_tokens)) AS source_cached_total".to_owned(),
        "countIf(ifNull(cache_read_reported, 0) = 1) AS source_read_n".to_owned(),
    ];
    for axis in ["prompt", "completion"] {
        for state in STATES {
            let condition = measured(axis, state);
            expressions.push(format!("countIf({condition}) AS source_{axis}_{state}_n"));
            expressions.push(format!(
                "sumIf(toUInt64({axis}_tokens), {condition}) AS source_{axis}_{state}_tokens"
            ));
        }
    }
    let cache = format!(
        "{} AND ifNull(cache_read_reported, 0) = 1 AND cached_tokens <= prompt_tokens",
        measured("prompt", "upstream")
    );
    expressions.push(format!("countIf({cache}) AS source_cache_n"));
    expressions.push(format!(
        "sumIf(toUInt64(prompt_tokens), {cache}) AS source_cache_prompt"
    ));
    expressions.push(format!(
        "sumIf(toUInt64(cached_tokens), {cache}) AS source_cache_read"
    ));
    expressions.join(", ")
}

pub(super) fn sum_sql() -> String {
    FIELDS
        .map(|field| format!("sum({field}) AS {field}"))
        .join(", ")
}

fn merged_sql() -> String {
    FIELDS
        .map(|field| {
            let expression = if field == "source_observed" {
                "countMerge(requests)".to_owned()
            } else if field.ends_with("_n") {
                format!("countIfMerge({field})")
            } else if field.ends_with("_total") {
                format!("sumMerge({field})")
            } else {
                format!("sumIfMerge({field})")
            };
            format!("{expression} AS {field}")
        })
        .join(", ")
}

/// All arguments are validated/internal SQL. Recover only missing grains in this scope.
pub(super) fn source(keys: &str, table: &str, predicate: &str) -> String {
    prepared(
        keys,
        table,
        predicate,
        super::measurement_coverage::Mode::Recover,
    )
}

pub(super) fn prepared(
    keys: &str,
    table: &str,
    predicate: &str,
    mode: super::measurement_coverage::Mode,
) -> String {
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
    let aggregate = merged_sql();
    let raw = raw_sql();
    if mode != super::measurement_coverage::Mode::Recover {
        let expected = format!(
            "WITH {time}, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS n FROM {table} WHERE {predicate} GROUP BY {keys}"
        );
        let counts = format!(
            "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS n FROM mv_usage_sources_5min WHERE {predicate} GROUP BY {keys}"
        );
        let aggregate = format!(
            "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, {aggregate} FROM mv_usage_sources_5min WHERE {predicate}"
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
    let selected = FIELDS.map(|field| format!("toInt64(if(use_aggregate, ifNull(a.{field}, 0), if(ifNull(r.source_observed, 0) <= e.expected, ifNull(r.{field}, 0), 0))) AS {field}")).join(", ");
    format!(
        "(WITH \
        e AS (SELECT {keys}, countMerge(requests) AS expected FROM (SELECT *, {time}, toUInt8(1) AS source_scope FROM {table} WHERE {predicate}) WHERE {predicate} GROUP BY {keys}), \
        a AS (SELECT {keys}, {aggregate} FROM (SELECT *, toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope FROM mv_usage_sources_5min) WHERE {predicate} GROUP BY {keys}), \
        missing AS (SELECT {keys} FROM e LEFT JOIN a USING ({keys}) WHERE e.expected != ifNull(a.source_observed, 0)), \
        r AS (SELECT {keys}, {raw} FROM (SELECT *, toStartOfFiveMinutes(ts) AS ts5, toStartOfHour(ts) AS hour, toDate(ts) AS day, toUInt8(1) AS source_scope FROM request_log_calls) raw_rows INNER JOIN missing USING ({keys}) WHERE {predicate} GROUP BY {keys}) \
        SELECT {selected_keys}, {selected}, e.expected AS source_expected, \
        (ifNull(a.source_observed, 0) <= e.expected AND ifNull(a.source_observed, 0) >= if(ifNull(r.source_observed, 0) <= e.expected, ifNull(r.source_observed, 0), 0)) AS use_aggregate \
        FROM e LEFT JOIN a USING ({keys}) LEFT JOIN r USING ({keys}))"
    )
}

fn ratio(part: i64, total: i64) -> Value {
    if total > 0 {
        json!(rate_bp(part, total))
    } else {
        Value::Null
    }
}

fn axis(row: &Value, name: &str, requests: i64, total: Option<i64>) -> Value {
    let mut result = json!({});
    let mut known = [0_i64; 2];
    for state in STATES {
        let n = ch_i64(row, &format!("source_{name}_{state}_n"));
        let tokens = ch_i64(row, &format!("source_{name}_{state}_tokens"));
        known[0] = known[0].saturating_add(n);
        known[1] = known[1].saturating_add(tokens);
        result[state] = json!({"requests":n,"tokens":tokens,"request_share_bp":ratio(n,requests),"token_share_bp":total.map_or(Value::Null, |t| ratio(tokens,t))});
    }
    result["unknown"] = json!({"requests":requests.saturating_sub(known[0]).max(0),
        "tokens":total.map(|t| t.saturating_sub(known[1]).max(0)),
        "request_share_bp":ratio(requests.saturating_sub(known[0]).max(0),requests),
        "token_share_bp":total.map_or(Value::Null, |t| ratio(t.saturating_sub(known[1]).max(0),t))});
    result
}

/// Axis totals can be unknown on old summary-only rows; do not invent their split.
pub(super) fn metrics(
    row: &Value,
    requests: i64,
    totals: [Option<i64>; 2],
    cached: Option<i64>,
    read_known: i64,
) -> Map<String, Value> {
    let observed = ch_i64(row, "source_observed");
    let complete = observed == requests;
    let prompt = totals[0].or_else(|| complete.then(|| ch_i64(row, "source_prompt_total")));
    let completion = totals[1].or_else(|| complete.then(|| ch_i64(row, "source_completion_total")));
    let cached = cached.or_else(|| complete.then(|| ch_i64(row, "source_cached_total")));
    let paired = ch_i64(row, "source_cache_n");
    let denominator = ch_i64(row, "source_cache_prompt");
    let cache_read = ch_i64(row, "source_cache_read");
    let subset = if paired > 0 {
        ratio(cache_read, denominator)
    } else {
        Value::Null
    };
    let settled = match (cached, prompt) {
        (Some(cached), Some(prompt)) => {
            super::usage_details::cache_rate(cached, prompt, requests, read_known)
        }
        _ => Value::Null,
    };
    let mut result = Map::new();
    result.insert("token_usage_basis".into(), json!("settled"));
    result.insert("token_provenance".into(), json!({"observed_requests":observed,"history_complete":complete,"history_coverage_bp":ratio(observed,requests),
        "prompt":axis(row,"prompt",requests,prompt),"completion":axis(row,"completion",requests,completion)}));
    result.insert(
        "cache_hit_bp".into(),
        if complete
            && paired == requests
            && requests > 0
            && prompt == Some(denominator)
            && cached == Some(cache_read)
        {
            subset.clone()
        } else {
            Value::Null
        },
    );
    result.insert("cache_hit_basis".into(), json!("upstream_prompt_tokens"));
    result.insert("measured_cache_hit_bp".into(), subset);
    result.insert("measured_cache_hit_requests".into(), json!(paired));
    result.insert(
        "measured_prompt_tokens".into(),
        if paired > 0 {
            json!(denominator)
        } else {
            Value::Null
        },
    );
    result.insert(
        "measured_cache_read_tokens".into(),
        if paired > 0 {
            json!(cache_read)
        } else {
            Value::Null
        },
    );
    result.insert(
        "measured_cache_hit_coverage_bp".into(),
        ratio(paired, requests),
    );
    result.insert("settled_cache_hit_bp".into(), settled);
    result
}

pub(super) fn accumulate(total: &mut Value, row: &Value) {
    for field in FIELDS {
        total[field] = json!(ch_i64(total, field).saturating_add(ch_i64(row, field)));
    }
}

fn key(row: &Value, columns: &str) -> Vec<String> {
    columns
        .split(", ")
        .map(|column| {
            let value = if column == "ts5" {
                row.get("ts5").unwrap_or(&row["bucket"])
            } else {
                &row[column]
            };
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned)
        })
        .collect()
}

/// Bound both legacy scans and aggregate reads to the displayed rows and same time scope.
pub(super) async fn enrich(
    ch: &ChClient,
    columns: &str,
    table: &str,
    predicate: &str,
    data: &mut [Value],
) -> Result<(), AppError> {
    if data.is_empty() {
        return Ok(());
    }
    let keys: Vec<_> = data.iter().map(|row| key(row, columns)).collect();
    let mut filters = Vec::new();
    let mut params = Vec::new();
    for (index, column) in columns.split(", ").enumerate() {
        let kind = match column {
            "channel_id" => "UInt32",
            "user_id" | "api_key_id" => "UInt64",
            "day" => "Date",
            "ts5" => "DateTime",
            _ => "String",
        };
        let values: HashSet<_> = keys.iter().map(|k| k[index].clone()).collect();
        let bindings: Vec<_> = values
            .into_iter()
            .enumerate()
            .map(|(n, value)| {
                let name = format!("source_key_{index}_{n}");
                let bind = format!("{{{name}:{kind}}}");
                params.push((name, value));
                bind
            })
            .collect();
        filters.push(format!("{column} IN ({})", bindings.join(",")));
    }
    let predicate = format!("{predicate} AND {}", filters.join(" AND "));
    let sql = super::token_details::with_provenance(columns, table, &predicate);
    let params: Vec<_> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let sources: HashMap<_, _> = ch
        .query_with_params(&format!("SELECT * FROM {sql}"), &params)
        .await?
        .into_iter()
        .map(|row| (key(&row, columns), row))
        .collect();
    for (row, k) in data.iter_mut().zip(keys) {
        let empty = json!({});
        let source = sources.get(&k).unwrap_or(&empty);
        super::input_units::correct_totals(row, source)?;
        let total = |name| {
            row.get(name)
                .filter(|v| !v.is_null())
                .map(|_| ch_i64(row, name))
        };
        let requests = ch_i64(row, "requests");
        let values = metrics(
            source,
            requests,
            [total("prompt_tokens"), total("completion_tokens")],
            total("cached_tokens"),
            ch_i64(source, "source_read_n"),
        );
        if let Some(object) = row.as_object_mut() {
            object.extend(values);
            object.extend(super::token_details::metrics(source, requests));
        }
    }
    Ok(())
}

/// PG ledger summary uses the same per-axis categories and explicitly reported cache pairs.
pub(super) fn pg_sql() -> String {
    let mut fields = vec!["'source_observed', COUNT(*)".to_owned()];
    for (field, column) in [
        ("source_prompt_total", "prompt_tokens"),
        ("source_completion_total", "completion_tokens"),
        ("source_cached_total", "cached_tokens"),
    ] {
        fields.push(format!("'{field}', COALESCE(SUM(b.{column}::bigint),0)"));
    }
    fields.push("'source_read_n', COUNT(*) FILTER (WHERE COALESCE((b.usage_details->'tokens'->>'cache_read_reported')::boolean, b.cached_tokens > 0))".to_owned());
    let predicate = |axis: &str, state: &str| {
        let original = format!("b.usage_details->'tokens'->'upstream_usage'->'{axis}_tokens'");
        let condition = match state {
            "upstream" => format!("{original} = to_jsonb(b.{axis}_tokens)"),
            "estimated" => format!("({original} IS NULL OR {original} = 'null'::jsonb)"),
            _ => format!(
                "jsonb_typeof({original})='number' AND {original} != to_jsonb(b.{axis}_tokens)"
            ),
        };
        format!("b.usage_details->>'{axis}_source' = '{state}' AND {condition}")
    };
    for axis in ["prompt", "completion"] {
        for state in STATES {
            let condition = predicate(axis, state);
            fields.push(format!(
                "'source_{axis}_{state}_n', COUNT(*) FILTER (WHERE {condition})"
            ));
            fields.push(format!("'source_{axis}_{state}_tokens', COALESCE(SUM(b.{axis}_tokens::bigint) FILTER (WHERE {condition}),0)"));
        }
    }
    let cache = format!(
        "{} AND (b.usage_details->'tokens'->>'cache_read_reported')::boolean AND b.cached_tokens <= b.prompt_tokens",
        predicate("prompt", "upstream")
    );
    fields.push(format!("'source_cache_n', COUNT(*) FILTER (WHERE {cache})"));
    for (field, column) in [
        ("source_cache_prompt", "prompt_tokens"),
        ("source_cache_read", "cached_tokens"),
    ] {
        fields.push(format!(
            "'{field}', COALESCE(SUM(b.{column}::bigint) FILTER (WHERE {cache}),0)"
        ));
    }
    fields.join(", ")
}
