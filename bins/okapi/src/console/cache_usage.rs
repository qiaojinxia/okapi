//! Cache quantities and sample counts always describe one selected population.
use super::measurement_coverage::Mode;

pub(super) const FIELDS: [&str; 4] = [
    "cache_observed",
    "cache_writes",
    "cache_write_n",
    "cache_read_n",
];
const RAW: &str = "count() AS cache_observed, sum(toUInt64(ifNull(cache_write_tokens, 0))) AS cache_writes, countIf(ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens)) AS cache_write_n, countIf(ifNull(cache_read_reported, 0) = 1) AS cache_read_n";
const MERGED: &str = "countMerge(requests) AS cache_observed, sumMerge(cache_writes) AS cache_writes, countIfMerge(cache_write_n) AS cache_write_n, countIfMerge(cache_read_n) AS cache_read_n";

pub(super) fn source(keys: &str, table: &str, predicate: &str) -> String {
    prepared(keys, table, predicate, Mode::Recover)
}

/// Validated/internal SQL only; strings in predicates remain server-bound.
pub(super) fn prepared(keys: &str, table: &str, predicate: &str, mode: Mode) -> String {
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
    let expected = format!(
        "WITH {time}, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS expected FROM {table} WHERE {predicate} GROUP BY {keys}"
    );
    let aggregate = format!(
        "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, {MERGED} FROM mv_cache_totals_5min WHERE {predicate}"
    );
    let raw = format!(
        "WITH toStartOfFiveMinutes(ts) AS ts5, toStartOfHour(ts) AS hour, toDate(ts) AS day, toUInt8(1) AS source_scope SELECT {keys}, {RAW} FROM request_log_raw WHERE {predicate}"
    );
    if mode != Mode::Recover {
        let counts = format!(
            "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS n FROM mv_cache_totals_5min WHERE {predicate} GROUP BY {keys}"
        );
        let expected_count = expected.replace(" AS expected", " AS n");
        let selected = super::measurement_coverage::fast_source(
            mode,
            keys,
            &expected_count,
            &counts,
            &aggregate,
            &raw,
        );
        return format!("(SELECT *, toInt64(cache_observed) AS cache_expected FROM {selected})");
    }
    recover(keys, table, predicate, &expected, &aggregate, &raw)
}

fn recover(
    keys: &str,
    table: &str,
    predicate: &str,
    expected: &str,
    aggregate: &str,
    raw: &str,
) -> String {
    let selected_keys = keys
        .split(", ")
        .map(|key| format!("e.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let bridge = super::cache_usage_legacy::bridge(keys, table, predicate);
    let selected = FIELDS.map(|field| format!("toInt64(multiIf(chosen = 1, ifNull(a.{field}, 0), chosen = 2, ifNull(r.{field}, 0), chosen = 3, ifNull(b.{field}, 0), 0)) AS {field}")).join(", ");
    let valid_count = |alias| {
        format!(
            "if(ifNull({alias}.cache_observed, 0) <= e.expected, ifNull({alias}.cache_observed, 0), 0)"
        )
    };
    let a = valid_count("a");
    let r = valid_count("r");
    let b = valid_count("b");
    format!(
        "(WITH e AS ({expected}), a AS ({aggregate} GROUP BY {keys}), \
        missing AS (SELECT {keys} FROM e LEFT JOIN a USING ({keys}) WHERE e.expected != ifNull(a.cache_observed, 0)), \
        r AS (SELECT raw_rows.* FROM ({raw} GROUP BY {keys}) raw_rows INNER JOIN missing USING ({keys})), \
        b AS ({bridge}) \
        SELECT {selected_keys}, {selected}, toInt64(e.expected) AS cache_expected, \
        multiIf({a} > 0 AND {a} >= {r} AND {a} >= {b}, 1, {r} > 0 AND {r} >= {b}, 2, {b} > 0, 3, 0) AS chosen \
        FROM e LEFT JOIN a USING ({keys}) LEFT JOIN r USING ({keys}) LEFT JOIN b USING ({keys}))"
    )
}
