-- Compact financial history survives configured deletion of detailed log partitions.
-- Numeric integer aggregates avoid overflow when opposing lifetime actor totals
-- exceed bigint although the final spendable balance still fits Money.
CREATE TABLE billing_event_carry (
    user_id BIGINT NOT NULL,
    pool SMALLINT NOT NULL CHECK (pool IN (0,1)),
    actor VARCHAR(64) NOT NULL,
    event_type VARCHAR(16) NOT NULL,
    delta_micro NUMERIC(38,0) NOT NULL,
    event_count BIGINT NOT NULL CHECK (event_count > 0),
    PRIMARY KEY (user_id,pool,actor,event_type)
);
CREATE VIEW billing_balance_totals AS
SELECT user_id,pool,SUM(delta_micro)::bigint AS delta_micro
FROM (
    SELECT user_id,pool,delta_micro::numeric AS delta_micro FROM billing_events
    UNION ALL
    SELECT user_id,pool,delta_micro FROM billing_event_carry
) facts GROUP BY user_id,pool;
CREATE VIEW billing_actor_totals AS
SELECT user_id,actor,SUM(delta_micro) AS delta_micro
FROM (
    SELECT user_id,actor,delta_micro::numeric AS delta_micro FROM billing_events
    UNION ALL
    SELECT user_id,actor,delta_micro FROM billing_event_carry
) facts GROUP BY user_id,actor;

-- Retain amounts, identities and financial explanation; omit request content,
-- IP/UA, performance telemetry and other expired log details.
CREATE TABLE billing_record_receipts (
    request_id UUID PRIMARY KEY,
    user_id BIGINT NOT NULL,
    api_key_id BIGINT,
    group_code VARCHAR(32),
    model_name VARCHAR(128) NOT NULL,
    channel_id BIGINT,
    channel_key_id BIGINT,
    status SMALLINT NOT NULL,
    amount_micro BIGINT NOT NULL,
    original_amount_micro BIGINT NOT NULL,
    discount_micro BIGINT NOT NULL,
    upstream_cost_micro BIGINT,
    is_stream BOOLEAN NOT NULL,
    node VARCHAR(64),
    pool SMALLINT NOT NULL CHECK (pool IN (0,1)),
    pricing_snapshot JSONB,
    usage_details JSONB,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX billing_receipts_user ON billing_record_receipts(user_id,created_at);
CREATE VIEW billing_financial_records AS
SELECT request_id,user_id,api_key_id,status,amount_micro,pool,created_at FROM billing_records
UNION ALL
SELECT request_id,user_id,api_key_id,status,amount_micro,pool,created_at FROM billing_record_receipts;
