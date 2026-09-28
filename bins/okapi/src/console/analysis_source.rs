//! 新维度聚合 + 旧立方体未覆盖部分。TTFT 仅在新聚合覆盖不足时恢复原始样本。
const KEYS: &str = "hour, user_id, api_key_id, group_code, model, channel_id";
const DIMS: &str = "requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type";
const METRICS: [(&str, &str); 16] = [
    ("requests", "countMerge"),
    ("prompt_tokens", "sumMerge"),
    ("cached_tokens", "sumMerge"),
    ("completion_tokens", "sumMerge"),
    ("reasoning_tokens", "sumMerge"),
    ("amount", "sumMerge"),
    ("discount", "sumMerge"),
    ("upstream_cost", "sumMerge"),
    ("errors", "sumMerge"),
    ("latency_sum", "sumMerge"),
    ("latency_samples", "countIfMerge"),
    ("latency_observed", "countMerge"),
    ("latency_output", "sumMerge"),
    ("ttft_sum", "sumMerge"),
    ("ttft_samples", "countIfMerge"),
    ("ttft_observed", "countMerge"),
];

/// `window` 和 `scope` 是已校验的 SQL，字符串过滤均由调用方绑定。
pub fn source(window: &str, scope: &str) -> String {
    let predicate = format!("{window}{scope}");
    let detail_ttft =
        super::ttft_average::source(&format!("{KEYS}, {DIMS}"), "mv_analysis_hour", &predicate);
    let legacy_ttft = super::ttft_average::source(KEYS, "mv_cube_hour", &predicate);
    let detail_latency =
        super::latency::source(&format!("{KEYS}, {DIMS}"), "mv_analysis_hour", &predicate);
    let legacy_latency = super::latency::source(KEYS, "mv_cube_hour", &predicate);
    let merged = METRICS
        .iter()
        .map(|(name, func)| {
            if name.starts_with("ttft_") {
                format!("toInt64(max(ifNull(tf.{name}, 0))) AS v_{name}")
            } else if name.starts_with("latency_") {
                format!("toInt64(max(ifNull(lf.{name}, 0))) AS v_{name}")
            } else {
                format!("toInt64({func}({name})) AS v_{name}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sums = METRICS
        .iter()
        .map(|(name, _)| format!("sum(v_{name}) AS c_{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let detailed = METRICS
        .iter()
        .map(|(name, _)| format!("v_{name} AS {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let remainder = METRICS
        .iter()
        .map(|(name, _)| {
            let difference = format!("l.v_{name} - ifNull(c.c_{name}, 0)");
            if let Some(prefix) = name.split_once('_').map(|(prefix, _)| prefix).filter(|prefix| matches!(*prefix, "ttft" | "latency")) {
                format!("if(l.v_{prefix}_observed = l.v_requests AND ifNull(c.c_{prefix}_observed, 0) = ifNull(c.c_requests, 0), {difference}, toInt64(0)) AS {name}")
            } else {
                format!("{difference} AS {name}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let legacy_keys = KEYS
        .split(", ")
        .map(|k| format!("l.{k} AS {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(WITH \
        d AS (SELECT {KEYS}, {DIMS}, {merged}, \
            toInt64(sumMerge(writes)) AS write_tokens, toInt64(max(ifNull(cr.write_n, 0))) AS write_samples, \
            toInt64(max(ifNull(cr.read_n, 0))) AS read_samples, \
            toInt64(countIfMerge(cost_known)) AS cost_samples, sumMerge(known_amount) AS covered_amount, sumMerge(known_cost) AS covered_cost, \
            maxMerge(last_event) AS event_at, maxMerge(last_ingested) AS ingested_at \
            FROM mv_analysis_hour LEFT JOIN {detail_latency} lf USING ({KEYS}, {DIMS}) LEFT JOIN {detail_ttft} tf USING ({KEYS}, {DIMS}) LEFT JOIN (SELECT {KEYS}, {DIMS}, \
                countIfMerge(read_known) AS read_n, countIfMerge(write_known) AS write_n \
                FROM mv_cache_reporting_hour WHERE {window}{scope} GROUP BY {KEYS}, {DIMS}) cr \
                USING ({KEYS}, {DIMS}) WHERE {window}{scope} GROUP BY {KEYS}, {DIMS}), \
        l AS (SELECT {KEYS}, {merged} FROM mv_cube_hour LEFT JOIN {legacy_latency} lf USING ({KEYS}) LEFT JOIN {legacy_ttft} tf USING ({KEYS}) WHERE {window}{scope} GROUP BY {KEYS}), \
        c AS (SELECT {KEYS}, {sums} FROM d GROUP BY {KEYS}) \
        SELECT {KEYS}, {DIMS}, {detailed}, write_tokens, write_samples, read_samples, cost_samples, covered_amount, covered_cost, event_at, ingested_at FROM d \
        UNION ALL \
        SELECT {legacy_keys}, '' AS requested_model, '' AS upstream_model, '' AS endpoint, '' AS upstream_endpoint, '' AS node, \
            toUInt8(2) AS stream, '' AS request_type, '' AS billing_type, {remainder}, \
            toInt64(0) AS write_tokens, toInt64(0) AS write_samples, toInt64(0) AS read_samples, toInt64(0) AS cost_samples, toInt64(0) AS covered_amount, toInt64(0) AS covered_cost, \
            toDateTime64(0, 3) AS event_at, toDateTime64(0, 3) AS ingested_at \
        FROM l LEFT JOIN c USING ({KEYS}) WHERE l.v_requests > ifNull(c.c_requests, 0))"
    )
}
