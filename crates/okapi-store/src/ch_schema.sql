-- ClickHouse schema（唯一权威：docs/database.md §3.1/§3.2）
-- 幂等：全部 IF NOT EXISTS；批次幂等依赖 non_replicated_deduplication_window + insert_deduplication_token
-- 收敛后的 5 张核心物化视图（+ server_tool_minute_v1 = 共 6 张物理 MV）：
--   1. mv_analysis_5min   — 14 维宽表（汇总全部核心指标、TTFT/时延、缓存、Token 来源/明细/TTL、输入单位、输出速率）
--   2. mv_calendar_minute — 6 维分钟级日历表（服务全部按日看板与任意分钟偏移本地时区边界）
--   3. mv_model_hour      — 模型小时级汇总与有效样本 TTFT/时延分位数
--   4. mv_channel_5min    — 渠道 5 分钟健康汇总、粘性/切换计数与有效样本 TTFT/时延分位数
--   5. mv_error_hour      — 失败请求错误码分布（WHERE is_error = 1）

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

-- 增量升级列：保持老表平滑升级；缺失观测保留 NULL，不冒充测得的 0。
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS prompt_source LowCardinality(String) DEFAULT 'unknown';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS completion_source LowCardinality(String) DEFAULT 'unknown';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_prompt_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_completion_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS ttft_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS diagnostics String DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS billing_status Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS requested_model LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_model LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS endpoint LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_endpoint LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS billing_type LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS request_type LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS upstream_cost_known UInt8 DEFAULT 0;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS pool UInt8 DEFAULT 0;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS ingested_at DateTime64(3, 'UTC') DEFAULT toDateTime64(0, 3);
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS latency_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_5m_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS server_tool_usage String DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_1h_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_prompt_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_prompt_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_completion_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_completion_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_audio_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_image_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_audio_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_image_tokens Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_prompt_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_prompt_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS audio_completion_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS image_completion_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_audio_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_read_image_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_audio_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS cache_write_image_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS reasoning_reported Nullable(UInt8) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS input_unit LowCardinality(String) DEFAULT '';
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS input_characters Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS historical_prompt_units Nullable(UInt32) DEFAULT NULL;
ALTER TABLE request_log_raw ADD COLUMN IF NOT EXISTS input_unit_basis LowCardinality(String) DEFAULT '';

-- 稀疏历史字符证据（旧语音计费校准）。FINAL 折叠重投，最大副本数在 raw 过期后保留证据。
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

-- 固定 UTC 时区元数据，不改变已存 Unix 时间戳。
ALTER TABLE request_log_raw MODIFY COLUMN ts DateTime64(3, 'UTC');
ALTER TABLE request_log_raw MODIFY COLUMN ingested_at DateTime64(3, 'UTC');
ALTER TABLE legacy_speech_units_v1 MODIFY COLUMN ts DateTime64(3, 'UTC');
ALTER TABLE legacy_speech_calibration_v1 MODIFY COLUMN cursor_ts DateTime64(3, 'UTC');

-- 1. 统一 14 维宽分析聚合（5 分钟粒度）：单表承载全部核心指标、性能、缓存、来源、模态、TTL、单位与速率。
-- 调用/观测列内置 log_type IN (2, 5) 过滤（退款 log_type=6 只动财务列，不造请求数或观测样本）；
-- financial_records / population_classified 用于读取守门视图在遇到未分类事件时 fail-closed。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_analysis_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (ts5, user_id, api_key_id, group_code, model, channel_id, requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfFiveMinutes(ts) AS ts5,
    user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type,
    countStateIf(log_type IN (2, 5)) AS requests,
    countState() AS financial_records,
    countStateIf(log_type IN (2, 5, 6)) AS population_classified,
    sumStateIf(toUInt64(r.prompt_tokens), log_type IN (2, 5)) AS prompt_tokens,
    sumStateIf(toUInt64(r.cached_tokens), log_type IN (2, 5)) AS cached_tokens,
    sumStateIf(toUInt64(r.completion_tokens), log_type IN (2, 5)) AS completion_tokens,
    sumStateIf(toUInt64(r.reasoning_tokens), log_type IN (2, 5)) AS reasoning_tokens,
    sumState(amount_micro) AS amount,
    sumState(discount_micro) AS discount,
    sumState(upstream_cost_micro) AS upstream_cost,
    sumStateIf(toUInt64(is_error), log_type IN (2, 5)) AS errors,
    countIfState(upstream_cost_known = 1) AS cost_known,
    sumState(if(upstream_cost_known = 1, amount_micro, toInt64(0))) AS known_amount,
    sumState(if(upstream_cost_known = 1, upstream_cost_micro, toInt64(0))) AS known_cost,
    maxState(ts) AS last_event,
    maxState(ingested_at) AS last_ingested,
    sumIfState(toUInt64(ttft_ms), (log_type IN (2, 5)) AND stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_sum,
    countIfState((log_type IN (2, 5)) AND stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_samples,
    sumIfState(toUInt64(latency_ms), (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_sum,
    countIfState((log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_samples,
    sumIfState(toUInt64(r.completion_tokens), (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_output,
    sumStateIf(toUInt64(ifNull(cache_write_tokens, 0)), log_type IN (2, 5)) AS cache_writes,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens)) AS cache_write_n,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_read_reported, 0) = 1) AS cache_read_n,
    countIfState((log_type IN (2, 5)) AND prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = r.prompt_tokens, 0)) AS source_prompt_upstream_n,
    sumIfState(toUInt64(r.prompt_tokens), (log_type IN (2, 5)) AND prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = r.prompt_tokens, 0)) AS source_prompt_upstream_tokens,
    countIfState((log_type IN (2, 5)) AND prompt_source = 'estimated' AND isNull(upstream_prompt_tokens)) AS source_prompt_estimated_n,
    sumIfState(toUInt64(r.prompt_tokens), (log_type IN (2, 5)) AND prompt_source = 'estimated' AND isNull(upstream_prompt_tokens)) AS source_prompt_estimated_tokens,
    countIfState((log_type IN (2, 5)) AND prompt_source = 'local_override' AND ifNull(upstream_prompt_tokens != r.prompt_tokens, 0)) AS source_prompt_local_override_n,
    sumIfState(toUInt64(r.prompt_tokens), (log_type IN (2, 5)) AND prompt_source = 'local_override' AND ifNull(upstream_prompt_tokens != r.prompt_tokens, 0)) AS source_prompt_local_override_tokens,
    countIfState((log_type IN (2, 5)) AND completion_source = 'upstream' AND ifNull(upstream_completion_tokens = r.completion_tokens, 0)) AS source_completion_upstream_n,
    sumIfState(toUInt64(r.completion_tokens), (log_type IN (2, 5)) AND completion_source = 'upstream' AND ifNull(upstream_completion_tokens = r.completion_tokens, 0)) AS source_completion_upstream_tokens,
    countIfState((log_type IN (2, 5)) AND completion_source = 'estimated' AND isNull(upstream_completion_tokens)) AS source_completion_estimated_n,
    sumIfState(toUInt64(r.completion_tokens), (log_type IN (2, 5)) AND completion_source = 'estimated' AND isNull(upstream_completion_tokens)) AS source_completion_estimated_tokens,
    countIfState((log_type IN (2, 5)) AND completion_source = 'local_override' AND ifNull(upstream_completion_tokens != r.completion_tokens, 0)) AS source_completion_local_override_n,
    sumIfState(toUInt64(r.completion_tokens), (log_type IN (2, 5)) AND completion_source = 'local_override' AND ifNull(upstream_completion_tokens != r.completion_tokens, 0)) AS source_completion_local_override_tokens,
    countIfState((log_type IN (2, 5)) AND prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = r.prompt_tokens, 0) AND ifNull(cache_read_reported, 0) = 1 AND r.cached_tokens <= r.prompt_tokens) AS source_cache_n,
    sumIfState(toUInt64(r.prompt_tokens), (log_type IN (2, 5)) AND prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = r.prompt_tokens, 0) AND ifNull(cache_read_reported, 0) = 1 AND r.cached_tokens <= r.prompt_tokens) AS source_cache_prompt,
    sumIfState(toUInt64(r.cached_tokens), (log_type IN (2, 5)) AND prompt_source = 'upstream' AND ifNull(upstream_prompt_tokens = r.prompt_tokens, 0) AND ifNull(cache_read_reported, 0) = 1 AND r.cached_tokens <= r.prompt_tokens) AS source_cache_read,
    sumIfState(toUInt64(ifNull(input_characters, 0)), (log_type IN (2, 5)) AND input_unit = 'characters' AND isNotNull(input_characters) AND r.prompt_tokens = 0 AND r.completion_tokens = 0 AND r.cached_tokens = 0 AND ifNull(cache_write_tokens, 0) = 0 AND r.reasoning_tokens = 0 AND ifNull(audio_prompt_tokens, 0) = 0 AND ifNull(audio_completion_tokens, 0) = 0 AND ifNull(image_prompt_tokens, 0) = 0 AND ifNull(image_completion_tokens, 0) = 0) AS unit_characters,
    countIfState((log_type IN (2, 5)) AND input_unit = 'characters' AND isNotNull(input_characters) AND r.prompt_tokens = 0 AND r.completion_tokens = 0 AND r.cached_tokens = 0 AND ifNull(cache_write_tokens, 0) = 0 AND r.reasoning_tokens = 0 AND ifNull(audio_prompt_tokens, 0) = 0 AND ifNull(audio_completion_tokens, 0) = 0 AND ifNull(image_prompt_tokens, 0) = 0 AND ifNull(image_completion_tokens, 0) = 0) AS unit_character_n,
    countIfState((log_type IN (2, 5)) AND input_unit = 'tokens' AND isNull(input_characters)) AS unit_token_n,
    sumIfState(toUInt64(ifNull(audio_prompt_tokens, 0)), (log_type IN (2, 5)) AND ifNull(audio_prompt_reported, 0) = 1 AND isNotNull(audio_prompt_tokens)) AS observed_audio_prompt_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(audio_prompt_reported, 0) = 1 AND isNotNull(audio_prompt_tokens)) AS observed_audio_prompt_tokens_n,
    sumIfState(toUInt64(ifNull(image_prompt_tokens, 0)), (log_type IN (2, 5)) AND ifNull(image_prompt_reported, 0) = 1 AND isNotNull(image_prompt_tokens)) AS observed_image_prompt_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(image_prompt_reported, 0) = 1 AND isNotNull(image_prompt_tokens)) AS observed_image_prompt_tokens_n,
    sumIfState(toUInt64(ifNull(audio_completion_tokens, 0)), (log_type IN (2, 5)) AND ifNull(audio_completion_reported, 0) = 1 AND isNotNull(audio_completion_tokens)) AS observed_audio_completion_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(audio_completion_reported, 0) = 1 AND isNotNull(audio_completion_tokens)) AS observed_audio_completion_tokens_n,
    sumIfState(toUInt64(ifNull(image_completion_tokens, 0)), (log_type IN (2, 5)) AND ifNull(image_completion_reported, 0) = 1 AND isNotNull(image_completion_tokens)) AS observed_image_completion_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(image_completion_reported, 0) = 1 AND isNotNull(image_completion_tokens)) AS observed_image_completion_tokens_n,
    sumIfState(toUInt64(ifNull(cache_read_audio_tokens, 0)), (log_type IN (2, 5)) AND ifNull(cache_read_audio_reported, 0) = 1 AND isNotNull(cache_read_audio_tokens)) AS observed_cache_read_audio_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_read_audio_reported, 0) = 1 AND isNotNull(cache_read_audio_tokens)) AS observed_cache_read_audio_tokens_n,
    sumIfState(toUInt64(ifNull(cache_read_image_tokens, 0)), (log_type IN (2, 5)) AND ifNull(cache_read_image_reported, 0) = 1 AND isNotNull(cache_read_image_tokens)) AS observed_cache_read_image_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_read_image_reported, 0) = 1 AND isNotNull(cache_read_image_tokens)) AS observed_cache_read_image_tokens_n,
    sumIfState(toUInt64(ifNull(cache_write_audio_tokens, 0)), (log_type IN (2, 5)) AND ifNull(cache_write_audio_reported, 0) = 1 AND isNotNull(cache_write_audio_tokens)) AS observed_cache_write_audio_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_write_audio_reported, 0) = 1 AND isNotNull(cache_write_audio_tokens)) AS observed_cache_write_audio_tokens_n,
    sumIfState(toUInt64(ifNull(cache_write_image_tokens, 0)), (log_type IN (2, 5)) AND ifNull(cache_write_image_reported, 0) = 1 AND isNotNull(cache_write_image_tokens)) AS observed_cache_write_image_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_write_image_reported, 0) = 1 AND isNotNull(cache_write_image_tokens)) AS observed_cache_write_image_tokens_n,
    sumIfState(toUInt64(ifNull(r.reasoning_tokens, 0)), (log_type IN (2, 5)) AND ifNull(reasoning_reported, 0) = 1 AND isNotNull(r.reasoning_tokens)) AS observed_reasoning_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(reasoning_reported, 0) = 1 AND isNotNull(r.reasoning_tokens)) AS observed_reasoning_tokens_n,
    sumIfState(toUInt64(ifNull(cache_write_5m_tokens, 0)), (log_type IN (2, 5)) AND ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_5m_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_5m_tokens_n,
    sumIfState(toUInt64(ifNull(cache_write_1h_tokens, 0)), (log_type IN (2, 5)) AND ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_1h_tokens,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_write_reported, 0) = 1 AND isNotNull(cache_write_tokens) AND isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens) AND toUInt64(ifNull(cache_write_5m_tokens, 0)) + toUInt64(ifNull(cache_write_1h_tokens, 0)) = toUInt64(ifNull(cache_write_tokens, 0))) AS observed_cache_write_1h_tokens_n,
    countIfState((log_type IN (2, 5)) AND ((input_unit = 'tokens' AND isNull(input_characters)) OR (input_unit = 'characters' AND isNotNull(input_characters) AND r.prompt_tokens = 0 AND r.completion_tokens = 0 AND r.cached_tokens = 0 AND ifNull(cache_write_tokens, 0) = 0 AND r.reasoning_tokens = 0 AND ifNull(audio_prompt_tokens, 0) = 0 AND ifNull(audio_completion_tokens, 0) = 0 AND ifNull(image_prompt_tokens, 0) = 0 AND ifNull(image_completion_tokens, 0) = 0))) AS output_rate_known,
    countIfState((log_type IN (2, 5)) AND input_unit = 'tokens' AND isNull(input_characters)) AS output_rate_tokens,
    countIfState((log_type IN (2, 5)) AND input_unit = 'tokens' AND isNull(input_characters) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS output_rate_samples,
    sumIfState(toUInt64(latency_ms), (log_type IN (2, 5)) AND input_unit = 'tokens' AND isNull(input_characters) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS output_rate_sum,
    sumIfState(toUInt64(r.completion_tokens), (log_type IN (2, 5)) AND input_unit = 'tokens' AND isNull(input_characters) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS output_rate_output
FROM request_log_raw AS r
GROUP BY ts5, user_id, api_key_id, group_code, model, channel_id,
    requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type;

-- 2. 分钟级日历事实：保留分数小时本地午夜边界，统一承载所有按日看板视图（user/apikey/group/model/client/cache）。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_calendar_minute
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(minute)
ORDER BY (minute, user_id, api_key_id, group_code, model, client_type)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    toStartOfMinute(ts) AS minute, user_id, api_key_id, group_code, model, client_type,
    countStateIf(log_type IN (2, 5)) AS requests,
    countState() AS financial_records,
    countStateIf(log_type IN (2, 5, 6)) AS population_classified,
    sumStateIf(toUInt64(r.prompt_tokens), log_type IN (2, 5)) AS prompt_tokens,
    sumStateIf(toUInt64(r.cached_tokens), log_type IN (2, 5)) AS cached_tokens,
    sumStateIf(toUInt64(r.completion_tokens), log_type IN (2, 5)) AS completion_tokens,
    sumStateIf(toUInt64(r.reasoning_tokens), log_type IN (2, 5)) AS reasoning_tokens,
    sumStateIf(toUInt64(r.prompt_tokens) + toUInt64(r.completion_tokens), log_type IN (2, 5)) AS tokens,
    sumState(amount_micro) AS amount,
    sumState(original_amount_micro) AS original,
    sumState(discount_micro) AS discount,
    sumState(upstream_cost_micro) AS upstream_cost,
    sumStateIf(toUInt64(is_error), log_type IN (2, 5)) AS errors,
    sumStateIf(toUInt64(ifNull(cache_write_tokens, 0)), log_type IN (2, 5)) AS write_tokens,
    countIfState((log_type IN (2, 5)) AND isNotNull(cache_write_tokens)) AS numeric_writes,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_read_reported, 0) = 1) AS read_known,
    countIfState((log_type IN (2, 5)) AND ifNull(cache_write_reported, 0) = 1) AS write_known
FROM request_log_raw AS r
GROUP BY minute, user_id, api_key_id, group_code, model, client_type;

-- 3. 模型小时级表现与有效样本分位数（合并原 mv_model_hour / mv_model_ttft_hour / mv_model_latency_hour）。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_model_hour
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(hour)
ORDER BY (model, hour)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    model,
    toStartOfHour(ts) AS hour,
    countStateIf(log_type IN (2, 5)) AS requests,
    countState() AS financial_records,
    countStateIf(log_type IN (2, 5, 6)) AS population_classified,
    sumStateIf(toUInt64(prompt_tokens) + toUInt64(completion_tokens), log_type IN (2, 5)) AS tokens,
    sumState(amount_micro) AS amount,
    sumStateIf(toUInt64(completion_tokens), log_type IN (2, 5)) AS completion_tokens_sum,
    sumStateIf(toUInt64(latency_ms), log_type IN (2, 5)) AS latency_sum,
    countIfState((log_type IN (2, 5)) AND stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_samples,
    quantilesIfState(0.5, 0.95, 0.99)(ttft_ms, (log_type IN (2, 5)) AND stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_q,
    sumIfState(toUInt64(latency_ms), (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_total_ms,
    countIfState((log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_samples,
    sumIfState(toUInt64(completion_tokens), (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_output_tokens,
    quantilesIfState(0.5, 0.95, 0.99)(latency_ms, (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_q
FROM request_log_raw
GROUP BY model, hour;

-- 4. 渠道 5 分钟健康与有效样本分位数（合并原 mv_channel_5min / mv_channel_ttft_5min / mv_channel_latency_5min）。
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_channel_5min
ENGINE = AggregatingMergeTree()
PARTITION BY toYYYYMM(ts5)
ORDER BY (channel_id, ts5)
SETTINGS non_replicated_deduplication_window = 1000
AS SELECT
    channel_id,
    toStartOfFiveMinutes(ts) AS ts5,
    countStateIf(log_type IN (2, 5)) AS requests,
    countState() AS financial_records,
    countStateIf(log_type IN (2, 5, 6)) AS population_classified,
    sumStateIf(toUInt64(is_error), log_type IN (2, 5)) AS errors,
    sumState(amount_micro) AS amount,
    sumState(upstream_cost_micro) AS upstream_cost,
    sumStateIf(toUInt64(completion_tokens), log_type IN (2, 5)) AS completion_tokens_sum,
    sumStateIf(toUInt64(latency_ms), log_type IN (2, 5)) AS latency_sum,
    sumStateIf(toUInt64(failover_count), log_type IN (2, 5)) AS failovers,
    countIfState((log_type IN (2, 5)) AND sticky_layer = 1) AS sticky_resp_hits,
    countIfState((log_type IN (2, 5)) AND sticky_layer = 2) AS sticky_sess_hits,
    countIfState((log_type IN (2, 5)) AND stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_samples,
    quantilesIfState(0.5, 0.95, 0.99)(ttft_ms, (log_type IN (2, 5)) AND stream = 1 AND ifNull(ttft_reported, toUInt8(ttft_ms > 0)) = 1) AS ttft_q,
    sumIfState(toUInt64(latency_ms), (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_total_ms,
    countIfState((log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_samples,
    sumIfState(toUInt64(completion_tokens), (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_output_tokens,
    quantilesIfState(0.5, 0.95, 0.99)(latency_ms, (log_type IN (2, 5)) AND ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1) AS latency_q
FROM request_log_raw
GROUP BY channel_id, ts5;

-- 5. 错误分布：只吃失败行（WHERE 在 MV 里即插入期过滤）。
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
    countStateIf(log_type IN (2, 5)) AS errors,
    maxStateIf(r.upstream_status, log_type IN (2, 5)) AS upstream_status,
    countState() AS financial_records,
    countStateIf(log_type IN (2, 5, 6)) AS population_classified
FROM request_log_raw AS r
WHERE is_error = 1
GROUP BY error_code, hour, channel_id, model;

-- 出口代理流量：每个进程每分钟每个代理一行（node 区分副本），读时 sum；只留 7 天。
CREATE TABLE IF NOT EXISTS egress_traffic_minute (
    minute     DateTime('UTC'),
    proxy_id   UInt64,
    node       LowCardinality(String),
    up_bytes   UInt64,
    down_bytes UInt64,
    requests   UInt64
) ENGINE = SummingMergeTree
PARTITION BY toYYYYMMDD(minute)
ORDER BY (proxy_id, minute, node)
TTL minute + INTERVAL 7 DAY
SETTINGS non_replicated_deduplication_window = 1000;
