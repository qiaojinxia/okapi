-- Keep lifetime channel usage independently of detailed billing retention.
-- Fence settlement and retention during the one-time historical backfill.
SELECT pg_advisory_xact_lock(87184753906516::bigint);
LOCK TABLE billing_records IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE billing_record_receipts IN SHARE MODE;

CREATE TABLE channel_token_totals (
    channel_id BIGINT PRIMARY KEY,
    tokens NUMERIC(38,0) NOT NULL CHECK (tokens >= 0)
);

-- Receipts retain the raw normalized token counts in usage_details. Malformed
-- historic values cannot cast-fail migration or become a fabricated token count.
CREATE FUNCTION channel_receipt_tokens(details JSONB) RETURNS BIGINT
LANGUAGE SQL IMMUTABLE PARALLEL SAFE AS $$
    SELECT
        CASE WHEN details #>> '{tokens,prompt_tokens}' ~ '^[0-9]{1,10}$'
             THEN (details #>> '{tokens,prompt_tokens}')::bigint ELSE 0 END
      + CASE WHEN details #>> '{tokens,completion_tokens}' ~ '^[0-9]{1,10}$'
             THEN (details #>> '{tokens,completion_tokens}')::bigint ELSE 0 END
$$;

INSERT INTO channel_token_totals(channel_id,tokens)
SELECT channel_id,SUM(tokens) FROM (
    SELECT channel_id,greatest(prompt_tokens,0)::bigint + greatest(completion_tokens,0)::bigint AS tokens
      FROM billing_records WHERE channel_id IS NOT NULL AND log_type IN (2,5)
    UNION ALL
    SELECT channel_id,channel_receipt_tokens(usage_details) FROM billing_record_receipts
      WHERE channel_id IS NOT NULL
) facts GROUP BY channel_id;

-- This runs in the billing transaction. Settlement replay skips an existing
-- request before INSERT, so totals and bills commit once together. Refunds and
-- retention deletion never undo upstream tokens already used.
CREATE FUNCTION record_channel_tokens() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.channel_id IS NOT NULL AND NEW.log_type IN (2,5) THEN
        INSERT INTO channel_token_totals(channel_id,tokens)
        VALUES (NEW.channel_id,greatest(NEW.prompt_tokens,0)::bigint + greatest(NEW.completion_tokens,0)::bigint)
        ON CONFLICT(channel_id) DO UPDATE SET tokens=channel_token_totals.tokens+EXCLUDED.tokens;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER billing_channel_tokens AFTER INSERT ON billing_records
FOR EACH ROW EXECUTE FUNCTION record_channel_tokens();

CREATE INDEX billing_receipts_channel_time ON billing_record_receipts(channel_id,created_at);
