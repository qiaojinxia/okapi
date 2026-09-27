-- Business state/event/snapshot and this intent commit together. Redis receipts
-- remain until applied_at is durable; cleaned_at makes interrupted cleanup retryable.
CREATE TABLE fund_transfers (
    id UUID PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    amount_micro BIGINT NOT NULL CHECK (amount_micro BETWEEN -9007199254740991 AND 9007199254740991),
    pool SMALLINT NOT NULL CHECK (pool IN (0, 1)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_at TIMESTAMPTZ,
    cleaned_at TIMESTAMPTZ,
    CHECK (cleaned_at IS NULL OR applied_at IS NOT NULL)
);
CREATE INDEX fund_transfers_pending ON fund_transfers(user_id,created_at,id) WHERE cleaned_at IS NULL;
