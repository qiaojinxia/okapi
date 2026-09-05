//! 新维度聚合 + 旧立方体未覆盖部分。只读聚合态，不扫描原始请求，也不回填或重复统计。
const KEYS: &str = "hour, user_id, api_key_id, group_code, model, channel_id";
const DIMS: &str = "requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type";
const METRICS: [(&str, &str); 12] = [
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
    ("ttft_sum", "sumMerge"),
    ("ttft_samples", "countIfMerge"),
];

/// `window` 和 `scope` 是已校验的 SQL，字符串过滤均由调用方绑定。
pub fn source(window: &str, scope: &str) -> String {
    let merged = METRICS
        .iter()
        .map(|(name, func)| format!("toInt64({func}({name})) AS v_{name}"))
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
        .map(|(name, _)| format!("l.v_{name} - ifNull(c.c_{name}, 0) AS {name}"))
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
            toInt64(sumMerge(writes)) AS write_tokens, toInt64(countIfMerge(writes_known)) AS write_samples, \
            toInt64(countIfMerge(cost_known)) AS cost_samples, sumMerge(known_amount) AS covered_amount, sumMerge(known_cost) AS covered_cost, \
            maxMerge(last_event) AS event_at, maxMerge(last_ingested) AS ingested_at \
            FROM mv_analysis_hour WHERE {window}{scope} GROUP BY {KEYS}, {DIMS}), \
        l AS (SELECT {KEYS}, {merged} FROM mv_cube_hour WHERE {window}{scope} GROUP BY {KEYS}), \
        c AS (SELECT {KEYS}, {sums} FROM d GROUP BY {KEYS}) \
        SELECT {KEYS}, {DIMS}, {detailed}, write_tokens, write_samples, cost_samples, covered_amount, covered_cost, event_at, ingested_at FROM d \
        UNION ALL \
        SELECT {legacy_keys}, '' AS requested_model, '' AS upstream_model, '' AS endpoint, '' AS upstream_endpoint, '' AS node, \
            toUInt8(2) AS stream, '' AS request_type, '' AS billing_type, {remainder}, \
            toInt64(0) AS write_tokens, toInt64(0) AS write_samples, toInt64(0) AS cost_samples, toInt64(0) AS covered_amount, toInt64(0) AS covered_cost, \
            toDateTime64(0, 3) AS event_at, toDateTime64(0, 3) AS ingested_at \
        FROM l LEFT JOIN c USING ({KEYS}) WHERE l.v_requests > ifNull(c.c_requests, 0))"
    )
}
