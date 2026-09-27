-- Durable PG-first completion of ordinary Redis reservations. The bill, event,
-- usage and this recovery row are committed in the same transaction.
CREATE TABLE billing_sync (
    request_id UUID PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    api_key_id BIGINT NOT NULL,
    amount_micro BIGINT NOT NULL CHECK (amount_micro BETWEEN 0 AND 9007199254740991),
    pool SMALLINT NOT NULL CHECK (pool IN (0, 1)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX billing_sync_user ON billing_sync(user_id, created_at, request_id);
