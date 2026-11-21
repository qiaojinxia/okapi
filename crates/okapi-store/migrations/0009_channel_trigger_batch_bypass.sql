-- 结算微批组提交（Group Commit）时通过事务级 GUC 跳过逐行触发器：
-- 微批事务会在内存按 channel_id / bucket_start 预聚合后，于 COMMIT 前一次性按主键升序 upsert
-- channel_token_totals 与 channel_usage_buckets，消除多请求并发命中主力渠道时的热点行锁放大。
-- 未设该 GUC 的单笔/裸 SQL 写入仍照旧由触发器维护，完全向后兼容。
CREATE OR REPLACE FUNCTION record_channel_tokens() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    v_tokens bigint;
    v_cost bigint;
    v_unknown bigint;
    v_bucket timestamptz;
BEGIN
    IF current_setting('okapi.skip_channel_trigger', true) = 'on' THEN
        RETURN NEW;
    END IF;
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
