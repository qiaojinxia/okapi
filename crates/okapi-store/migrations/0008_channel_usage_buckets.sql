-- 渠道周期用量的 15 分钟 UTC 物化桶（channel_usage_buckets）：
-- - 全球所有 IANA 时区（含 +05:30 / +05:45 与夏令时切换）的自然小时 / 日 / 周 / 月边界均为 15 分钟整倍数，
--   查询任意时区窗口 [window_start, window_end) 时直接按主键 (channel_id, bucket_start) 做有界范围求和：
--   日 <= 96 行、周 <= 672 行、月 <= 2976 行，避免每次选路扫描整窗 billing_records + billing_record_receipts
--   并免除 history::read 归档锁竞争。
-- - 与 channel_token_totals 一样不挂 channels 外键、不随 billing_records 归档删除，确保归档前后读数一致；
--   超 45 天的旧桶由 worker 的 channel_usage::prune 分批清理。
CREATE TABLE channel_usage_buckets (
    channel_id bigint NOT NULL,
    bucket_start timestamptz NOT NULL,
    requests bigint DEFAULT 0 NOT NULL,
    tokens numeric(38,0) DEFAULT 0 NOT NULL,
    cost_micro numeric(38,0) DEFAULT 0 NOT NULL,
    unknown_cost_requests bigint DEFAULT 0 NOT NULL,
    CONSTRAINT channel_usage_buckets_pkey PRIMARY KEY (channel_id, bucket_start),
    CONSTRAINT channel_usage_buckets_requests_check CHECK (requests >= 0),
    CONSTRAINT channel_usage_buckets_tokens_check CHECK (tokens >= (0)::numeric),
    CONSTRAINT channel_usage_buckets_cost_check CHECK (cost_micro >= (0)::numeric),
    CONSTRAINT channel_usage_buckets_unknown_check CHECK (unknown_cost_requests >= 0)
);

CREATE INDEX channel_usage_buckets_expiry ON channel_usage_buckets (bucket_start);

COMMENT ON TABLE channel_usage_buckets IS '渠道 15 分钟 UTC 增量用量桶，支撑任意 IANA 时区下 O(桶数) 的小时/日/周/月上限判定';

CREATE OR REPLACE FUNCTION record_channel_tokens() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    v_tokens bigint;
    v_cost bigint;
    v_unknown bigint;
    v_bucket timestamptz;
BEGIN
    IF NEW.channel_id IS NOT NULL AND NEW.log_type IN (2, 5) THEN
        v_tokens := greatest(NEW.prompt_tokens, 0)::bigint + greatest(NEW.completion_tokens, 0)::bigint;
        v_cost := greatest(COALESCE(NEW.upstream_cost_micro, 0), 0);
        v_unknown := CASE WHEN NEW.upstream_cost_micro IS NULL AND NEW.log_type = 2 THEN 1 ELSE 0 END;
        v_bucket := date_bin(interval '15 minutes', NEW.created_at, timestamptz '2000-01-01 00:00:00+00');

        INSERT INTO channel_token_totals(channel_id, tokens, cost_micro)
        VALUES (NEW.channel_id, v_tokens, v_cost)
        ON CONFLICT(channel_id) DO UPDATE SET
            tokens = channel_token_totals.tokens + EXCLUDED.tokens,
            cost_micro = channel_token_totals.cost_micro + EXCLUDED.cost_micro;

        INSERT INTO channel_usage_buckets(channel_id, bucket_start, requests, tokens, cost_micro, unknown_cost_requests)
        VALUES (NEW.channel_id, v_bucket, 1, v_tokens, v_cost, v_unknown)
        ON CONFLICT(channel_id, bucket_start) DO UPDATE SET
            requests = channel_usage_buckets.requests + 1,
            tokens = channel_usage_buckets.tokens + EXCLUDED.tokens,
            cost_micro = channel_usage_buckets.cost_micro + EXCLUDED.cost_micro,
            unknown_cost_requests = channel_usage_buckets.unknown_cost_requests + EXCLUDED.unknown_cost_requests;
    END IF;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION move_channel_usage_bucket() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    v_tokens bigint;
    v_cost bigint;
    v_unknown bigint;
    v_old_bucket timestamptz;
    v_new_bucket timestamptz;
BEGIN
    IF OLD.channel_id IS NOT NULL AND OLD.log_type IN (2, 5) AND OLD.created_at IS DISTINCT FROM NEW.created_at THEN
        v_old_bucket := date_bin(interval '15 minutes', OLD.created_at, timestamptz '2000-01-01 00:00:00+00');
        v_new_bucket := date_bin(interval '15 minutes', NEW.created_at, timestamptz '2000-01-01 00:00:00+00');
        IF v_old_bucket <> v_new_bucket THEN
            v_tokens := greatest(OLD.prompt_tokens, 0)::bigint + greatest(OLD.completion_tokens, 0)::bigint;
            v_cost := greatest(COALESCE(OLD.upstream_cost_micro, 0), 0);
            v_unknown := CASE WHEN OLD.upstream_cost_micro IS NULL AND OLD.log_type = 2 THEN 1 ELSE 0 END;

            UPDATE channel_usage_buckets
               SET requests = greatest(requests - 1, 0),
                   tokens = greatest(tokens - v_tokens, 0),
                   cost_micro = greatest(cost_micro - v_cost, 0),
                   unknown_cost_requests = greatest(unknown_cost_requests - v_unknown, 0)
             WHERE channel_id = OLD.channel_id
               AND bucket_start = v_old_bucket;

            INSERT INTO channel_usage_buckets(channel_id, bucket_start, requests, tokens, cost_micro, unknown_cost_requests)
            VALUES (OLD.channel_id, v_new_bucket, 1, v_tokens, v_cost, v_unknown)
            ON CONFLICT(channel_id, bucket_start) DO UPDATE SET
                requests = channel_usage_buckets.requests + 1,
                tokens = channel_usage_buckets.tokens + EXCLUDED.tokens,
                cost_micro = channel_usage_buckets.cost_micro + EXCLUDED.cost_micro,
                unknown_cost_requests = channel_usage_buckets.unknown_cost_requests + EXCLUDED.unknown_cost_requests;
        END IF;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER billing_channel_usage_bucket_move
    AFTER UPDATE OF created_at ON billing_records
    FOR EACH ROW EXECUTE FUNCTION move_channel_usage_bucket();

-- 回填最近 45 天已落账记录（现存账单 + 归档回执），与 prune 保留期一致。
INSERT INTO channel_usage_buckets(channel_id, bucket_start, requests, tokens, cost_micro, unknown_cost_requests)
SELECT history.channel_id,
       date_bin(interval '15 minutes', history.created_at, timestamptz '2000-01-01 00:00:00+00') AS bucket_start,
       COUNT(*)::bigint AS requests,
       SUM(history.tokens) AS tokens,
       SUM(history.cost) AS cost_micro,
       SUM(history.unknown)::bigint AS unknown_cost_requests
  FROM (
        SELECT channel_id,
               created_at,
               (greatest(prompt_tokens, 0)::bigint + greatest(completion_tokens, 0)::bigint)::numeric AS tokens,
               greatest(COALESCE(upstream_cost_micro, 0), 0)::numeric AS cost,
               CASE WHEN upstream_cost_micro IS NULL AND log_type = 2 THEN 1 ELSE 0 END AS unknown
          FROM billing_records
         WHERE channel_id IS NOT NULL
           AND log_type IN (2, 5)
           AND created_at >= now() - interval '45 days'
        UNION ALL
        SELECT channel_id,
               created_at,
               channel_receipt_tokens(usage_details)::numeric AS tokens,
               greatest(COALESCE(upstream_cost_micro, 0), 0)::numeric AS cost,
               CASE WHEN upstream_cost_micro IS NULL THEN 1 ELSE 0 END AS unknown
          FROM billing_record_receipts
         WHERE channel_id IS NOT NULL
           AND created_at >= now() - interval '45 days'
       ) history
 GROUP BY history.channel_id,
          date_bin(interval '15 minutes', history.created_at, timestamptz '2000-01-01 00:00:00+00')
ON CONFLICT(channel_id, bucket_start) DO UPDATE SET
    requests = EXCLUDED.requests,
    tokens = EXCLUDED.tokens,
    cost_micro = EXCLUDED.cost_micro,
    unknown_cost_requests = EXCLUDED.unknown_cost_requests;
