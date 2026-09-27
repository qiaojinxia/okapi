-- Durable reservations for provider jobs that outlive synchronous request deadlines.
-- Never cascade away recovery/settlement evidence while remote work may still exist.
CREATE TABLE balance_holds (
    id UUID PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    api_key_id BIGINT NOT NULL REFERENCES api_keys(id),
    model_name TEXT NOT NULL,
    request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
    maximum_micro BIGINT NOT NULL CHECK (maximum_micro BETWEEN 0 AND 9007199254740991),
    pricing_snapshot JSONB NOT NULL CHECK (jsonb_typeof(pricing_snapshot) = 'object'),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'held', 'closing', 'closed')),
    pool SMALLINT CHECK (pool IN (0, 1)),
    source_window TEXT,
    actual_micro BIGINT CHECK (actual_micro BETWEEN 0 AND maximum_micro),
    credit_micro BIGINT CHECK (credit_micro BETWEEN 0 AND maximum_micro),
    settlement JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (octet_length(pricing_snapshot::text) <= 65536),
    CHECK ((state = 'pending') = (pool IS NULL)),
    CHECK ((pool = 1) = (source_window IS NOT NULL)),
    CHECK ((state IN ('closing', 'closed')) = (settlement IS NOT NULL)),
    CHECK ((state IN ('closing', 'closed')) = (actual_micro IS NOT NULL)),
    CHECK ((state IN ('closing', 'closed')) = (credit_micro IS NOT NULL))
);
CREATE INDEX balance_holds_active ON balance_holds(user_id, created_at, id) WHERE state <> 'closed';
CREATE INDEX balance_holds_closing ON balance_holds(updated_at, id) WHERE state = 'closing';
