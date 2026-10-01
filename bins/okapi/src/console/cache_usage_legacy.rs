//! A retained day sum cannot be arbitrarily spread across hours/channels.
const PARENT: &str = "parent_day, parent_user, parent_key, parent_model";
const PARENT_COLUMNS: &str = "toDate(hour) AS parent_day, user_id AS parent_user, api_key_id AS parent_key, model AS parent_model";

pub(super) fn bridge(keys: &str, table: &str, predicate: &str) -> String {
    let columns: Vec<_> = keys.split(", ").collect();
    // The old cube has no five-minute or newer endpoint/node/model dimensions.
    if table == "mv_analysis_hour"
        || columns.iter().any(|key| {
            !matches!(
                *key,
                "hour"
                    | "day"
                    | "user_id"
                    | "api_key_id"
                    | "model"
                    | "channel_id"
                    | "group_code"
                    | "source_scope"
            )
        })
    {
        return format!(
            "SELECT {keys}, toUInt64(0) AS cache_observed, toUInt64(0) AS cache_writes, toUInt64(0) AS cache_write_n, toUInt64(0) AS cache_read_n FROM e WHERE 0"
        );
    }
    let aliased = columns
        .iter()
        .enumerate()
        .map(|(i, key)| format!("{key} AS k{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let child = (0..columns.len())
        .map(|i| format!("k{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let selected = columns
        .iter()
        .enumerate()
        .map(|(i, key)| format!("k{i} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    // The complete parent count intentionally has no channel/group/time predicate:
    // a filtered subset must not inherit the entire day's cache amount.
    format!(
        "WITH \
        m AS (WITH toDate(hour) AS day, toUInt8(1) AS source_scope SELECT {PARENT_COLUMNS}, {aliased}, countMerge(requests) AS matching_n FROM mv_cube_hour WHERE {predicate} GROUP BY {PARENT}, {child}), \
        u AS (SELECT {PARENT}, count() AS children FROM m GROUP BY {PARENT}), \
        p AS (SELECT {PARENT_COLUMNS}, countMerge(requests) AS parent_n FROM mv_cube_hour WHERE ({PARENT}) IN (SELECT {PARENT} FROM m) GROUP BY {PARENT}), \
        w AS (SELECT day AS parent_day, user_id AS parent_user, api_key_id AS parent_key, model AS parent_model, sumMerge(write_tokens) AS writes, countIfMerge(known_requests) AS numeric_n FROM mv_cache_write_day WHERE ({PARENT}) IN (SELECT {PARENT} FROM m) GROUP BY {PARENT}), \
        o AS (SELECT day AS parent_day, user_id AS parent_user, api_key_id AS parent_key, model AS parent_model, countIfMerge(write_known) AS write_n, countIfMerge(read_known) AS read_n FROM mv_cache_reporting_day WHERE ({PARENT}) IN (SELECT {PARENT} FROM m) GROUP BY {PARENT}) \
        SELECT {selected}, sum(parent_n) AS cache_observed, sum(if(ifNull(w.numeric_n, 0) = parent_n, ifNull(w.writes, 0), 0)) AS cache_writes, \
            sum(if(ifNull(w.numeric_n, 0) = parent_n, ifNull(o.write_n, 0), 0)) AS cache_write_n, sum(ifNull(o.read_n, 0)) AS cache_read_n \
        FROM m INNER JOIN u USING ({PARENT}) INNER JOIN p USING ({PARENT}) LEFT JOIN w USING ({PARENT}) LEFT JOIN o USING ({PARENT}) \
        WHERE children = 1 AND matching_n = parent_n AND parent_n > 0 AND ifNull(w.numeric_n, 0) <= parent_n AND ifNull(o.write_n, 0) <= parent_n AND ifNull(o.read_n, 0) <= parent_n GROUP BY {child}"
    )
}
