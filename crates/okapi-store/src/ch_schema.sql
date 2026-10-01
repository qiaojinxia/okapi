-- ClickHouse schema（唯一权威：docs/database.md §3.1/§3.2）
-- 幂等：全部 IF NOT EXISTS；批次幂等依赖 non_replicated_deduplication_window + insert_deduplication_token

CREATE TABLE IF NOT EXISTS request_log_raw (
    ts              DateTime64(3, 'UTC'),
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
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_reported Nullable(UInt8) DEFAULT NULL;
-- Unknown provenance is not upstream usage; NULL per-axis counts remain missing.
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS prompt_source LowCardinality(String) DEFAULT 'unknown';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS completion_source LowCardinality(String) DEFAULT 'unknown';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_prompt_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_completion_tokens Nullable(UInt32) DEFAULT NULL;
-- NULL 是旧行：只有正 TTFT 可确认采集；新行显式区分测得 0 与未采集。
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS ttft_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS diagnostics String DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS billing_status Nullable(UInt8) DEFAULT NULL;

-- 新聚合保留所有请求的覆盖数，但分位数只收流式且已采集的样本。
-- 不替换旧 MV、不 POPULATE：历史缺口由查询侧在 raw 完整时重算，避免重复计数。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_model_ttft_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (model, hour)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    model, toStartOfHour(ts) AS hour,
    countState() AS requests,
    countIfState(stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS samples,
    quantilesIfState(0.5, 0.95, 0.99)(ttft_ms,
        stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_q
FROM request_log_raw
GROUP BY model, hour;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_channel_ttft_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (channel_id, ts5)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    channel_id, toStartOfFiveMinutes(ts) AS ts5,
    countState() AS requests,
    countIfState(stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS samples,
    quantilesIfState(0.5, 0.95, 0.99)(ttft_ms,
        stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_q
FROM request_log_raw
GROUP BY channel_id, ts5;

-- 不回填历史：旧零值无法判断是未命中还是未上报。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_cache_reporting_day
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(day)
ORDER BY (user_id, api_key_id, model, day)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    user_id, api_key_id, model, toDate(ts) AS day,
    countIfState(ifNull(cache_read_reported, 0) = 1) AS read_known,
    countIfState(ifNull(cache_write_reported, 0) = 1) AS write_known,
    sumState(toUInt64(ifNull(cache_write_tokens, 0))) AS write_tokens
FROM request_log_raw
GROUP BY user_id, api_key_id, model, day;

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
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS pool UInt8 DEFAULT 0;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS ingested_at DateTime64(3, 'UTC') DEFAULT toDateTime64(0, 3);

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

-- 独立采集覆盖聚合，不覆盖既有分析视图或历史金额。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_cache_reporting_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (hour, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfHour(ts) AS hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countIfState(ifNull(cache_read_reported, 0) = 1) AS read_known,
    countIfState(ifNull(cache_write_reported, 0) = 1) AS write_known
FROM request_log_raw
GROUP BY hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- 平均首字时间的有效样本与分位数一致；独立新 MV 保留升级前的全部旧统计。
-- 不 POPULATE：查询在旧请求覆盖不足时择一恢复 raw，禁止重叠相加。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_ttft_reporting_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (hour, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfHour(ts) AS hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumIfState(toUInt64(ttft_ms), stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS total_ms,
    countIfState(stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS samples
FROM request_log_raw
GROUP BY hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- NULL denotes a legacy row; positive legacy durations are known, old zero is ambiguous.
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS latency_reported Nullable(UInt8) DEFAULT NULL;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_latency_reporting_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (hour, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfHour(ts) AS hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumIfState(toUInt64(latency_ms), ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS total_ms,
    countIfState(ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS samples,
    sumIfState(toUInt64(completion_tokens), ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS output_tokens
FROM request_log_raw
GROUP BY hour, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_model_latency_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (model, hour)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    model, toStartOfHour(ts) AS hour, countState() AS requests,
    sumIfState(toUInt64(latency_ms), ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS total_ms,
    countIfState(ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS samples,
    sumIfState(toUInt64(completion_tokens), ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS output_tokens,
    quantilesIfState(0.5, 0.95, 0.99)(latency_ms, ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_q
FROM request_log_raw GROUP BY model, hour;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_channel_latency_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (channel_id, ts5)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    channel_id, toStartOfFiveMinutes(ts) AS ts5, countState() AS requests,
    sumIfState(toUInt64(latency_ms), ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS total_ms,
    countIfState(ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS samples,
    sumIfState(toUInt64(completion_tokens), ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS output_tokens,
    quantilesIfState(0.5, 0.95, 0.99)(latency_ms, ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_q
FROM request_log_raw GROUP BY channel_id, ts5;

-- Provider totals and settlement totals retain independent provenance after raw retention.
-- Optional observations: old outbox payloads stay NULL, never synthetic zero.
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_5m_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_1h_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_prompt_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_prompt_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_completion_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_completion_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_audio_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_image_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_audio_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_image_tokens Nullable(UInt32) DEFAULT NULL;

-- Fine-grained observation flags distinguish unreported counters from measured zero.
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_prompt_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_prompt_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_completion_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_completion_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_audio_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_image_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_audio_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_image_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS reasoning_reported Nullable(UInt8) DEFAULT NULL;

-- No POPULATE: historical recovery chooses one complete source, never adds overlapping rows.
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_usage_sources_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfFiveMinutes(ts) AS ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens)) AS source_prompt_total,
    sumState(toUInt64(completion_tokens)) AS source_completion_total,
    sumState(toUInt64(cached_tokens)) AS source_cached_total,
    countIfState(ifNull(cache_read_reported, 0) = 1) AS source_read_n,
    countIfState(prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = prompt_tokens, 0)) AS source_prompt_upstream_n,
    sumIfState(toUInt64(prompt_tokens), prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = prompt_tokens, 0)) AS source_prompt_upstream_tokens,
    countIfState(prompt_source = 'estimated' AND isNull(upstream_prompt_tokens)) AS source_prompt_estimated_n,
    sumIfState(toUInt64(prompt_tokens), prompt_source = 'estimated' AND isNull(upstream_prompt_tokens)) AS source_prompt_estimated_tokens,
    countIfState(prompt_source = 'local_override' AND ifNull(upstream_prompt_tokens != prompt_tokens, 0)) AS source_prompt_local_override_n,
    sumIfState(toUInt64(prompt_tokens), prompt_source = 'local_override' AND ifNull(upstream_prompt_tokens != prompt_tokens, 0)) AS source_prompt_local_override_tokens,
    countIfState(completion_source = 'upstream' AND ifNull(upstream_completion_tokens = completion_tokens, 0)) AS source_completion_upstream_n,
    sumIfState(toUInt64(completion_tokens), completion_source = 'upstream' AND ifNull(upstream_completion_tokens = completion_tokens, 0)) AS source_completion_upstream_tokens,
    countIfState(completion_source = 'estimated' AND isNull(upstream_completion_tokens)) AS source_completion_estimated_n,
    sumIfState(toUInt64(completion_tokens), completion_source = 'estimated' AND isNull(upstream_completion_tokens)) AS source_completion_estimated_tokens,
    countIfState(completion_source = 'local_override' AND ifNull(upstream_completion_tokens != completion_tokens, 0)) AS source_completion_local_override_n,
    sumIfState(toUInt64(completion_tokens), completion_source = 'local_override' AND ifNull(upstream_completion_tokens != completion_tokens, 0)) AS source_completion_local_override_tokens,
    countIfState(prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = prompt_tokens, 0) AND ifNull(cache_read_reported, 0) = 1 AND cached_tokens <= prompt_tokens) AS source_cache_n,
    sumIfState(toUInt64(prompt_tokens), prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = prompt_tokens, 0) AND ifNull(cache_read_reported, 0) = 1 AND cached_tokens <= prompt_tokens) AS source_cache_prompt,
    sumIfState(toUInt64(cached_tokens), prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = prompt_tokens, 0) AND ifNull(cache_read_reported, 0) = 1 AND cached_tokens <= prompt_tokens) AS source_cache_read
FROM request_log_raw
GROUP BY ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- Sparse historical character evidence. FINAL collapses retries; the greatest
-- observed duplicate count survives partial raw expiry. No financial mutation.
CREATE TABLE IF NOT EXISTS legacy_speech_units_v1 (
    ts DateTime64(3, 'UTC'), request_id UUID,
    user_id UInt64, api_key_id UInt64, group_code LowCardinality(String),
    model LowCardinality(String), channel_id UInt32, channel_key_id UInt32,
    requested_model LowCardinality(String), upstream_model LowCardinality(String),
    endpoint LowCardinality(String), upstream_endpoint LowCardinality(String),
    node LowCardinality(String), stream UInt8, request_type LowCardinality(String),
    billing_type LowCardinality(String), log_type UInt8, is_error UInt8,
    client_type LowCardinality(String), provider LowCardinality(String),
    characters UInt32, snapshot_epoch UInt64, ratio_snapshot String,
    basis LowCardinality(String), copies UInt64
) ENGINE = ReplacingMergeTree(copies)
PARTITION BY toYYYYMM(ts)
ORDER BY (ts, request_id, user_id, api_key_id, group_code, model, channel_id,
    channel_key_id, requested_model, upstream_model, endpoint, upstream_endpoint,
    node, stream, request_type, billing_type, log_type, is_error, client_type,
    provider, characters, snapshot_epoch)
SETTINGS non_replicated_deduplication_window = 1000;

CREATE TABLE IF NOT EXISTS legacy_speech_calibration_v1 (
    slot UInt8, cursor_ts DateTime64(3, 'UTC'), cursor_id UUID, complete UInt8, version UInt64
) ENGINE = ReplacingMergeTree(version)
ORDER BY slot
SETTINGS non_replicated_deduplication_window = 1000;

-- Retain quantities and reporting counts from the same request population.
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS input_unit LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS input_characters Nullable(UInt32) DEFAULT NULL;
-- Old outbox rows are interpreted before all MVs consume them; keep their original
-- character carrier and the interpretation basis for raw log audits.
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS historical_prompt_units Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS input_unit_basis LowCardinality(String) DEFAULT '';

-- Independent physical units survive raw retention without rewriting old Token/money views.
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_input_units_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfFiveMinutes(ts) AS ts5, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumIfState(toUInt64(ifNull(input_characters, 0)), input_unit = 'characters' AND isNotNull(input_characters) AND prompt_tokens = 0 AND completion_tokens = 0 AND cached_tokens = 0 AND ifNull(cache_write_tokens, 0) = 0 AND reasoning_tokens = 0 AND ifNull(audio_prompt_tokens, 0) = 0 AND ifNull(audio_completion_tokens, 0) = 0 AND ifNull(image_prompt_tokens, 0) = 0 AND ifNull(image_completion_tokens, 0) = 0) AS unit_characters,
    countIfState(input_unit = 'characters' AND isNotNull(input_characters) AND prompt_tokens = 0 AND completion_tokens = 0 AND cached_tokens = 0 AND ifNull(cache_write_tokens, 0) = 0 AND reasoning_tokens = 0 AND ifNull(audio_prompt_tokens, 0) = 0 AND ifNull(audio_completion_tokens, 0) = 0 AND ifNull(image_prompt_tokens, 0) = 0 AND ifNull(image_completion_tokens, 0) = 0) AS unit_character_n,
    countIfState(input_unit = 'tokens' AND isNull(input_characters)) AS unit_token_n
FROM request_log_raw
GROUP BY ts5, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_cache_totals_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfFiveMinutes(ts) AS ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumState(toUInt64(ifNull(cache_write_tokens, 0))) AS cache_writes,
    countIfState(ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens)) AS cache_write_n,
    countIfState(ifNull(cache_read_reported, 0) = 1) AS cache_read_n
FROM request_log_raw
GROUP BY ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- Retained measured detail sums. Missing flags/values never become observed zeros.
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_token_details_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfFiveMinutes(ts) AS ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumIfState(toUInt64(ifNull(audio_prompt_tokens, 0)), ifNull(audio_prompt_reported, 0) = 1 AND isNotNull(audio_prompt_tokens)) AS observed_audio_prompt_tokens,
    countIfState(ifNull(audio_prompt_reported, 0) = 1 AND isNotNull(audio_prompt_tokens)) AS observed_audio_prompt_tokens_n,
    sumIfState(toUInt64(ifNull(image_prompt_tokens, 0)), ifNull(image_prompt_reported, 0) = 1 AND isNotNull(image_prompt_tokens)) AS observed_image_prompt_tokens,
    countIfState(ifNull(image_prompt_reported, 0) = 1 AND isNotNull(image_prompt_tokens)) AS observed_image_prompt_tokens_n,
    sumIfState(toUInt64(ifNull(audio_completion_tokens, 0)), ifNull(audio_completion_reported, 0) = 1 AND isNotNull(audio_completion_tokens)) AS observed_audio_completion_tokens,
    countIfState(ifNull(audio_completion_reported, 0) = 1 AND isNotNull(audio_completion_tokens)) AS observed_audio_completion_tokens_n,
    sumIfState(toUInt64(ifNull(image_completion_tokens, 0)), ifNull(image_completion_reported, 0) = 1 AND isNotNull(image_completion_tokens)) AS observed_image_completion_tokens,
    countIfState(ifNull(image_completion_reported, 0) = 1 AND isNotNull(image_completion_tokens)) AS observed_image_completion_tokens_n,
    sumIfState(toUInt64(ifNull(cache_read_audio_tokens, 0)), ifNull(cache_read_audio_reported, 0) = 1 AND isNotNull(cache_read_audio_tokens)) AS observed_cache_read_audio_tokens,
    countIfState(ifNull(cache_read_audio_reported, 0) = 1 AND isNotNull(cache_read_audio_tokens)) AS observed_cache_read_audio_tokens_n,
    sumIfState(toUInt64(ifNull(cache_read_image_tokens, 0)), ifNull(cache_read_image_reported, 0) = 1 AND isNotNull(cache_read_image_tokens)) AS observed_cache_read_image_tokens,
    countIfState(ifNull(cache_read_image_reported, 0) = 1 AND isNotNull(cache_read_image_tokens)) AS observed_cache_read_image_tokens_n,
    sumIfState(toUInt64(ifNull(cache_write_audio_tokens, 0)), ifNull(cache_write_audio_reported, 0) = 1 AND isNotNull(cache_write_audio_tokens)) AS observed_cache_write_audio_tokens,
    countIfState(ifNull(cache_write_audio_reported, 0) = 1 AND isNotNull(cache_write_audio_tokens)) AS observed_cache_write_audio_tokens_n,
    sumIfState(toUInt64(ifNull(cache_write_image_tokens, 0)), ifNull(cache_write_image_reported, 0) = 1 AND isNotNull(cache_write_image_tokens)) AS observed_cache_write_image_tokens,
    countIfState(ifNull(cache_write_image_reported, 0) = 1 AND isNotNull(cache_write_image_tokens)) AS observed_cache_write_image_tokens_n,
    sumIfState(toUInt64(ifNull(reasoning_tokens, 0)), ifNull(reasoning_reported, 0) = 1 AND isNotNull(reasoning_tokens)) AS observed_reasoning_tokens,
    countIfState(ifNull(reasoning_reported, 0) = 1 AND isNotNull(reasoning_tokens)) AS observed_reasoning_tokens_n
FROM request_log_raw
GROUP BY ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- Cache lifetime observations are independent of previously retained modality details.
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_cache_ttl_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfFiveMinutes(ts) AS ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    sumIfState(toUInt64(ifNull(cache_write_5m_tokens, 0)), ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_5m_tokens,
    countIfState(ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_5m_tokens_n,
    sumIfState(toUInt64(ifNull(cache_write_1h_tokens, 0)), ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_1h_tokens,
    countIfState(ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_1h_tokens_n
FROM request_log_raw
GROUP BY ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- Token output and duration share a unit-aware sample population. Character
-- requests remain in general latency aggregates, never in this denominator.
-- No POPULATE or raw TTL: historical gaps are recovered by bounded reads.
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_output_rate_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfFiveMinutes(ts) AS ts5, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countState() AS requests,
    countIfState((input_unit = 'tokens' AND isNull(input_characters)) OR
        (input_unit = 'characters' AND isNotNull(input_characters) AND prompt_tokens = 0 AND completion_tokens = 0 AND cached_tokens = 0 AND ifNull(cache_write_tokens, 0) = 0 AND reasoning_tokens = 0 AND ifNull(audio_prompt_tokens, 0) = 0 AND ifNull(audio_completion_tokens, 0) = 0 AND ifNull(image_prompt_tokens, 0) = 0 AND ifNull(image_completion_tokens, 0) = 0)) AS known_units,
    countIfState(input_unit = 'tokens' AND isNull(input_characters)) AS token_requests,
    countIfState(input_unit = 'tokens' AND isNull(input_characters) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS samples,
    sumIfState(toUInt64(latency_ms), input_unit = 'tokens' AND isNull(input_characters) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS total_ms,
    sumIfState(toUInt64(completion_tokens), input_unit = 'tokens' AND isNull(input_characters) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS output_tokens
FROM request_log_raw
GROUP BY ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- Fix timezone metadata without changing stored Unix timestamps.
ALTER TABLE request_log_raw MODIFY COLUMN ts DateTime64(3, 'UTC');
ALTER TABLE request_log_raw MODIFY COLUMN ingested_at DateTime64(3, 'UTC');
ALTER TABLE legacy_speech_units_v1 MODIFY COLUMN ts DateTime64(3, 'UTC');
ALTER TABLE legacy_speech_calibration_v1 MODIFY COLUMN cursor_ts DateTime64(3, 'UTC');


-- Retain calendar dimensions at hourly grain so machine timezone changes do not relabel UTC days.
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_calendar_client_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (client_type, hour)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    client_type,
    toStartOfHour(ts) AS hour,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens) + toUInt64(completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(toUInt64(is_error)) AS errors,
    uniqState(user_id) AS users
FROM request_log_raw
GROUP BY client_type, hour;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_calendar_cache_write_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (user_id, api_key_id, model, hour)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    user_id,
    api_key_id,
    model,
    toStartOfHour(ts) AS hour,
    sumState(toUInt64(ifNull(cache_write_tokens, 0))) AS write_tokens,
    countIfState(isNotNull(cache_write_tokens)) AS known_requests
FROM request_log_raw
GROUP BY user_id, api_key_id, model, hour;

-- Minute calendar facts preserve fractional-hour local midnight boundaries.
-- Independent upgrade: no POPULATE, no TTL, no changes to existing aggregates.
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_calendar_minute
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(minute)
ORDER BY (minute, user_id, api_key_id, group_code, model, client_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfMinute(ts) AS minute, user_id, api_key_id, group_code, model, client_type,
    countState() AS requests,
    sumState(toUInt64(prompt_tokens)) AS prompt_tokens,
    sumState(toUInt64(cached_tokens)) AS cached_tokens,
    sumState(toUInt64(completion_tokens)) AS completion_tokens,
    sumState(toUInt64(reasoning_tokens)) AS reasoning_tokens,
    sumState(toUInt64(r.prompt_tokens) + toUInt64(r.completion_tokens)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(original_amount_micro) AS original,
    sumState(discount_micro) AS discount,
    sumState(upstream_cost_micro) AS upstream_cost,
    sumState(toUInt64(is_error)) AS errors,
    sumState(toUInt64(ifNull(cache_write_tokens, 0))) AS write_tokens,
    countIfState(isNotNull(cache_write_tokens)) AS numeric_writes,
    countIfState(ifNull(cache_read_reported, 0) = 1) AS read_known,
    countIfState(ifNull(cache_write_reported, 0) = 1) AS write_known
FROM request_log_raw AS r
GROUP BY minute, user_id, api_key_id, group_code, model, client_type;

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_calendar_cache_reporting_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (user_id, api_key_id, model, hour)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    user_id, api_key_id, model, toStartOfHour(ts) AS hour,
    countIfState(ifNull(cache_read_reported, 0) = 1) AS read_known,
    countIfState(ifNull(cache_write_reported, 0) = 1) AS write_known,
    sumState(toUInt64(ifNull(cache_write_tokens, 0))) AS write_tokens
FROM request_log_raw
GROUP BY user_id, api_key_id, model, hour;
