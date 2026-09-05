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

-- 增量升级：历史缓存写入未采集时为 NULL；通过样本覆盖数区分未知与零。
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_tokens Nullable(UInt32) DEFAULT NULL;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_cache_write_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (user_id, api_key_id, model, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    user_id,
    api_key_id,
    model,
    toDate(ts) AS day,
    sumState(toUInt64(ifNull(cache_write_tokens, 0))) AS write_tokens,
    countIfState(isNotNull(cache_write_tokens)) AS known_requests
FROM request_log_raw
GROUP BY user_id, api_key_id, model, day;

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

-- 错误分布：只吃失败行（WHERE 在 MV 里即插入期过滤），故行数 =
-- 错误码 × 小时 × 渠道 × 模型，与总请求量无关。
-- 存在的理由：「错误率 3%」不可行动，「3% 里九成是某渠道的 429」才可行动；
-- 而 error_code 只在 raw 明细里，没有这张 MV 就得扫全分区才能出分布。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_error_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (error_code, hour, channel_id, model)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    error_code,
    toStartOfHour(ts) AS hour,
    channel_id,
    model,
    countState() AS errors,
    maxState(upstream_status) AS upstream_status
FROM request_log_raw
WHERE is_error = 1
GROUP BY error_code, hour, channel_id, model;

-- 门户明细维度（user × key × model × day）+ token 四轴构成。
-- 服务用户门户的三张图：按模型堆叠趋势 / 模型分布 / Token 构成——且在
-- key 视角（合作商员工只见自己那把 key）下同样成立；此前 mv_user_model_day
-- 只按用户，key 视角没有任何按模型拆分。主键前缀 (user_id, api_key_id) 使
-- 两种视角都是前缀扫描。行数 ∝ 活跃 key × 当日用到的模型数，与请求量无关。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_key_model_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (user_id, api_key_id, model, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    user_id,
    api_key_id,
    model,
    toDate(ts) AS day,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens)) AS prompt_tokens,
    sumState(toUInt64(cached_tokens)) AS cached_tokens,
    sumState(toUInt64(completion_tokens)) AS completion_tokens,
    sumState(toUInt64(reasoning_tokens)) AS reasoning_tokens,
    sumState(amount_micro) AS amount,
    sumState(discount_micro) AS discount,
    sumState(toUInt64(is_error)) AS errors
FROM request_log_raw
GROUP BY user_id, api_key_id, model, day;

-- 客户端类型分布（#5277）：UA 解析列按日聚合。uniqState(user_id) 让
-- "多少用户在用 Claude Code"可答——这比请求数更能说明生态渗透。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_client_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (client_type, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    client_type,
    toDate(ts) AS day,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens) + toUInt64(completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(toUInt64(is_error)) AS errors,
    uniqState(user_id) AS users
FROM request_log_raw
GROUP BY client_type, day;

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

-- 分析立方体（IMPLEMENTATION §11.13）：hour × user × key × group × model × channel。
-- 上面的单维 MV 各答一个固定问题，答不了"这个用户走了哪些渠道""这条渠道上
-- 谁在用什么模型"这类**带过滤的任意维度组合**（new-api #7150 / Sub2API TrendParams
-- 的诉求；new-api 的 quota_data 八维表就是同一思路）。行数 ∝ 每小时出现过的
-- (user,key,group,model,channel) 组合数，上界是请求数、实际远小于它；
-- 主键以 hour 开头让时间窗裁剪先生效。刻意不放 quantilesState：每行一个 sketch
-- 在这种基数下代价太高，时延只留和（avg = sum / n），分位数仍走 mv_model_hour /
-- mv_channel_5min。provider 是 channel_id 的函数，查询时从 PG 回填，不进键。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_cube_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (hour, user_id, api_key_id, group_code, model, channel_id)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfHour(ts) AS hour,
    user_id,
    api_key_id,
    group_code,
    model,
    channel_id,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens)) AS prompt_tokens,
    sumState(toUInt64(cached_tokens)) AS cached_tokens,
    sumState(toUInt64(completion_tokens)) AS completion_tokens,
    sumState(toUInt64(reasoning_tokens)) AS reasoning_tokens,
    sumState(amount_micro) AS amount,
    sumState(discount_micro) AS discount,
    sumState(upstream_cost_micro) AS upstream_cost,
    sumState(toUInt64(is_error)) AS errors,
    sumState(toUInt64(latency_ms)) AS latency_sum,
    sumState(toUInt64(ttft_ms)) AS ttft_sum,
    countIfState(ttft_ms > 0) AS ttft_samples
FROM request_log_raw
GROUP BY hour, user_id, api_key_id, group_code, model, channel_id;

-- 分析附加维度；不改旧聚合键，升级不会丢失旧统计。
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS requested_model LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_model LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS endpoint LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_endpoint LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS billing_type LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS request_type LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_cost_known UInt8 DEFAULT 0;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS ingested_at DateTime64(3) DEFAULT toDateTime64(0, 3);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_analysis_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (hour, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfHour(ts) AS hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens)) AS prompt_tokens,
    sumState(toUInt64(cached_tokens)) AS cached_tokens,
    sumState(toUInt64(completion_tokens)) AS completion_tokens,
    sumState(toUInt64(reasoning_tokens)) AS reasoning_tokens,
    sumState(amount_micro) AS amount,
    sumState(discount_micro) AS discount,
    sumState(upstream_cost_micro) AS upstream_cost,
    sumState(toUInt64(is_error)) AS errors,
    sumState(toUInt64(latency_ms)) AS latency_sum,
    sumState(toUInt64(ttft_ms)) AS ttft_sum,
    countIfState(ttft_ms > 0) AS ttft_samples,
    sumState(toUInt64(ifNull(cache_write_tokens, 0))) AS writes,
    countIfState(isNotNull(cache_write_tokens)) AS writes_known,
    countIfState(upstream_cost_known = 1) AS cost_known,
    sumState(if(upstream_cost_known = 1, amount_micro, toInt64(0))) AS known_amount,
    sumState(if(upstream_cost_known = 1, upstream_cost_micro, toInt64(0))) AS known_cost,
    maxState(ts) AS last_event,
    maxState(ingested_at) AS last_ingested
FROM request_log_raw
GROUP BY hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;
