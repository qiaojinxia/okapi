-- 用户日志汇总（portal/usage_logs.rs 的 stat）原先每行从 usage_details 取 30 个值、引用 280 次，
-- 每次都要取 JSON、判类型、正则校验再转型：线上 5 千行就要 3.2 秒，只取一次 JSON 也还要 2.3 秒，
-- 且随行数线性增长。这些值写入时由 PG 算一次存成列（与 CH 事件表同名同义），汇总只读列。
--
-- 守卫与原先查询侧一致：数字只认不越界的 JSON 整数，布尔只认 JSON 布尔，其余（字符串、小数、
-- 缺失、JSON null）一律 NULL = 未上报，一条脏记录不会让汇总报错。
-- 生成列随 usage_details 或所在行更新自动重算；分区父表上加列会递归到现有分区，
-- 之后 PARTITION OF 建的新分区也带上。加列时整表按表达式回填一次。
ALTER TABLE billing_records
    ADD COLUMN cache_read_reported boolean GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_read_reported') = 'boolean'
             THEN (usage_details->'tokens'->'cache_read_reported' #>> '{}')::boolean END) STORED,
    ADD COLUMN cache_write_reported boolean GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_write_reported') = 'boolean'
             THEN (usage_details->'tokens'->'cache_write_reported' #>> '{}')::boolean END) STORED,
    ADD COLUMN cache_write_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_write_tokens') = 'number'
              AND (usage_details->'tokens'->'cache_write_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'cache_write_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN cache_write_5m_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_write_5m_tokens') = 'number'
              AND (usage_details->'tokens'->'cache_write_5m_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'cache_write_5m_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN cache_write_1h_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_write_1h_tokens') = 'number'
              AND (usage_details->'tokens'->'cache_write_1h_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'cache_write_1h_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN audio_prompt_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'audio_prompt_tokens') = 'number'
              AND (usage_details->'tokens'->'audio_prompt_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'audio_prompt_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN image_prompt_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'image_prompt_tokens') = 'number'
              AND (usage_details->'tokens'->'image_prompt_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'image_prompt_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN audio_completion_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'audio_completion_tokens') = 'number'
              AND (usage_details->'tokens'->'audio_completion_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'audio_completion_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN image_completion_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'image_completion_tokens') = 'number'
              AND (usage_details->'tokens'->'image_completion_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'image_completion_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN cache_read_audio_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_read_modalities'->'audio_tokens') = 'number'
              AND (usage_details->'tokens'->'cache_read_modalities'->'audio_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'cache_read_modalities'->'audio_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN cache_read_image_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_read_modalities'->'image_tokens') = 'number'
              AND (usage_details->'tokens'->'cache_read_modalities'->'image_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'cache_read_modalities'->'image_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN cache_write_audio_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_write_modalities'->'audio_tokens') = 'number'
              AND (usage_details->'tokens'->'cache_write_modalities'->'audio_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'cache_write_modalities'->'audio_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN cache_write_image_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'cache_write_modalities'->'image_tokens') = 'number'
              AND (usage_details->'tokens'->'cache_write_modalities'->'image_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'cache_write_modalities'->'image_tokens' #>> '{}')::bigint END) STORED,
    -- 明细是否上报（reported_details）：只有 JSON true 算上报
    ADD COLUMN audio_prompt_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'prompt'->'audio' = 'true'::jsonb) STORED,
    ADD COLUMN image_prompt_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'prompt'->'image' = 'true'::jsonb) STORED,
    ADD COLUMN audio_completion_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'completion'->'audio' = 'true'::jsonb) STORED,
    ADD COLUMN image_completion_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'completion'->'image' = 'true'::jsonb) STORED,
    ADD COLUMN cache_read_audio_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'cache_read'->'audio' = 'true'::jsonb) STORED,
    ADD COLUMN cache_read_image_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'cache_read'->'image' = 'true'::jsonb) STORED,
    ADD COLUMN cache_write_audio_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'cache_write'->'audio' = 'true'::jsonb) STORED,
    ADD COLUMN cache_write_image_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'cache_write'->'image' = 'true'::jsonb) STORED,
    ADD COLUMN reasoning_reported boolean GENERATED ALWAYS AS (
        usage_details->'tokens'->'reported_details'->'reasoning' = 'true'::jsonb) STORED,
    -- 上游原始用量：与 prompt_tokens / completion_tokens 比出来源（上游 / 估算 / 本地改写）
    ADD COLUMN upstream_prompt_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'upstream_usage'->'prompt_tokens') = 'number'
              AND (usage_details->'tokens'->'upstream_usage'->'prompt_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'upstream_usage'->'prompt_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN upstream_completion_tokens bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'tokens'->'upstream_usage'->'completion_tokens') = 'number'
              AND (usage_details->'tokens'->'upstream_usage'->'completion_tokens' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'tokens'->'upstream_usage'->'completion_tokens' #>> '{}')::bigint END) STORED,
    ADD COLUMN prompt_source text GENERATED ALWAYS AS (usage_details->>'prompt_source') STORED,
    ADD COLUMN completion_source text GENERATED ALWAYS AS (usage_details->>'completion_source') STORED,
    -- 计量单位：按 token 计的请求写的是 input_characters: null，JSON null 也是 NULL
    ADD COLUMN input_unit text GENERATED ALWAYS AS (usage_details->>'input_unit') STORED,
    ADD COLUMN input_characters bigint GENERATED ALWAYS AS (
        CASE WHEN jsonb_typeof(usage_details->'input_characters') = 'number'
              AND (usage_details->'input_characters' #>> '{}') ~ '^-?[0-9]{1,18}$'
             THEN (usage_details->'input_characters' #>> '{}')::bigint END) STORED,
    ADD COLUMN request_failed boolean GENERATED ALWAYS AS (
        usage_details->'diagnostics'->>'request_failed' = 'true') STORED;
