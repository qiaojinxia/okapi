-- ClickHouse schema（唯一权威：docs/database.md §3.1/§3.2）
-- 幂等：全部 IF NOT EXISTS；批次幂等依赖 non_replicated_deduplication_window + insert_deduplication_token

CREATE TABLE IF NOT EXISTS request_log_raw (
    ts              DateTime64(3),
    request_id      UUID,
    upstream_request_id String,
    log_type        UInt8,
    user_id         UInt64,
    api_key_id      UInt64,
    team_id         UInt64 DEFAULT 0,
    group_code      LowCardinality(String),
    model           LowCardinality(String),
    channel_id      UInt32,
    channel_key_id  UInt32,
    provider        LowCardinality(String),
    client_type     LowCardinality(String),
    client_ip       String,
    node            LowCardinality(String),
    prompt_tokens     UInt32,
    cached_tokens     UInt32,
    completion_tokens UInt32,
    reasoning_tokens  UInt32,
    media_units       String,
    amount_micro          Int64,
    original_amount_micro Int64,
    discount_micro        Int64,
    upstream_cost_micro   Int64,
    pricing_epoch         UInt64,
    ratio_snapshot        String,
    latency_ms UInt32,
    ttft_ms    UInt32,
    stream     UInt8,
    retry_count    UInt8,
    failover_count UInt8,
    sticky_layer   UInt8,
    upstream_status UInt16,
    error_code LowCardinality(String),
    is_error   UInt8
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(ts)
ORDER BY (user_id, ts)
TTL toDateTime(ts) + INTERVAL 180 DAY
SETTINGS index_granularity = 8192, non_replicated_deduplication_window = 1000;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_user_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (user_id, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    user_id,
    toDate(ts) AS day,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens) + toUInt64(completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(original_amount_micro) AS original,
    sumState(discount_micro) AS discount,
    sumState(upstream_cost_micro) AS upstream_cost,
    sumState(toUInt64(is_error)) AS errors
FROM request_log_raw
GROUP BY user_id, day;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_apikey_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (api_key_id, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    api_key_id,
    toDate(ts) AS day,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens) + toUInt64(completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(discount_micro) AS discount,
    sumState(toUInt64(is_error)) AS errors
FROM request_log_raw
GROUP BY api_key_id, day;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_model_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (model, hour)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    model,
    toStartOfHour(ts) AS hour,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens) + toUInt64(completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    quantilesState(0.5, 0.95, 0.99)(latency_ms) AS latency_q,
    quantilesState(0.5, 0.95, 0.99)(ttft_ms) AS ttft_q,
    sumState(toUInt64(completion_tokens)) AS completion_tokens_sum,
    sumState(toUInt64(latency_ms)) AS latency_sum
FROM request_log_raw
GROUP BY model, hour;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_group_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (group_code, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    group_code,
    toDate(ts) AS day,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens) + toUInt64(completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(discount_micro) AS discount,
    sumState(toUInt64(is_error)) AS errors
FROM request_log_raw
GROUP BY group_code, day;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_channel_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (channel_id, ts5)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    channel_id,
    toStartOfFiveMinutes(ts) AS ts5,
    countState() AS requests,
    sumState(toUInt64(is_error)) AS errors,
    sumState(amount_micro) AS amount,
    sumState(upstream_cost_micro) AS upstream_cost,
    quantilesState(0.5, 0.95, 0.99)(ttft_ms) AS ttft_q,
    quantilesState(0.5, 0.95)(latency_ms) AS latency_q,
    sumState(toUInt64(completion_tokens)) AS completion_tokens_sum,
    sumState(toUInt64(latency_ms)) AS latency_sum,
    sumState(toUInt64(failover_count)) AS failovers,
    countIfState(sticky_layer = 1) AS sticky_resp_hits,
    countIfState(sticky_layer = 2) AS sticky_sess_hits
FROM request_log_raw
GROUP BY channel_id, ts5;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_user_model_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (user_id, model, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    user_id,
    model,
    toDate(ts) AS day,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens) + toUInt64(completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(discount_micro) AS discount
FROM request_log_raw
GROUP BY user_id, model, day;
