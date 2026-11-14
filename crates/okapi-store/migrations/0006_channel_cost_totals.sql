-- 渠道的累计上游成本，和累计 token 记在同一行、由同一个触发器维护。
-- 渠道的成本上限（channels.settings.account_control.local_cost）在 period = total 时读它：
-- 账单过了保留期会搬进回执表，一条请求一条地现加，量会一直涨，而放行判定每个请求都要读。
ALTER TABLE channel_token_totals
    ADD COLUMN cost_micro numeric(38,0) DEFAULT 0 NOT NULL,
    ADD CONSTRAINT channel_token_totals_cost_check CHECK ((cost_micro >= (0)::numeric));
COMMENT ON COLUMN channel_token_totals.cost_micro IS '该渠道成功（2）与流式中断（5）记录的 upstream_cost_micro 之和；成本未知（NULL）的记录不计';

CREATE OR REPLACE FUNCTION record_channel_tokens() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.channel_id IS NOT NULL AND NEW.log_type IN (2,5) THEN
        INSERT INTO channel_token_totals(channel_id,tokens,cost_micro)
        VALUES (NEW.channel_id,
                greatest(NEW.prompt_tokens,0)::bigint + greatest(NEW.completion_tokens,0)::bigint,
                greatest(COALESCE(NEW.upstream_cost_micro,0),0))
        ON CONFLICT(channel_id) DO UPDATE SET
            tokens=channel_token_totals.tokens+EXCLUDED.tokens,
            cost_micro=channel_token_totals.cost_micro+EXCLUDED.cost_micro;
    END IF;
    RETURN NEW;
END
$$;

-- 已有的账单补进累计值：还留着的账单加上归档回执（回执不带 log_type，但只有成功与流式中断的记录才有成本）。
INSERT INTO channel_token_totals(channel_id,tokens,cost_micro)
SELECT history.channel_id, 0, SUM(history.cost)
  FROM (
        SELECT channel_id, greatest(upstream_cost_micro,0)::numeric AS cost FROM billing_records
         WHERE channel_id IS NOT NULL AND log_type IN (2,5) AND upstream_cost_micro IS NOT NULL
        UNION ALL
        SELECT channel_id, greatest(upstream_cost_micro,0)::numeric FROM billing_record_receipts
         WHERE channel_id IS NOT NULL AND upstream_cost_micro IS NOT NULL
       ) history
 GROUP BY history.channel_id
ON CONFLICT(channel_id) DO UPDATE SET cost_micro=EXCLUDED.cost_micro;
