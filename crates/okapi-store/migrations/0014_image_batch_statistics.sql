-- Historical tasks have no reliable admission-time member snapshot. Leave NULL;
-- never infer past attribution from the current API key owner.
ALTER TABLE image_batches ADD COLUMN member_user_id BIGINT;
ALTER TABLE image_batches ADD COLUMN results_ready_at TIMESTAMPTZ;

-- Emitted atomically with first publication. Independent leases let delivery
-- retry after publication or artifact cleanup without repeating settlement.
CREATE TABLE image_batch_statistics (
    batch_id UUID PRIMARY KEY REFERENCES image_batches(id),
    user_id BIGINT NOT NULL,
    member_user_id BIGINT,
    channel_key_id BIGINT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    tokens BIGINT NOT NULL CHECK (tokens >= 0),
    amount_micro BIGINT NOT NULL CHECK (amount_micro BETWEEN 0 AND 9007199254740991),
    is_error BOOLEAN NOT NULL,
    delivered_at TIMESTAMPTZ,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_id UUID,
    lease_until TIMESTAMPTZ,
    CHECK ((lease_id IS NULL) = (lease_until IS NULL))
);
CREATE INDEX image_batch_statistics_pending ON image_batch_statistics(next_attempt_at,batch_id)
    WHERE delivered_at IS NULL;
