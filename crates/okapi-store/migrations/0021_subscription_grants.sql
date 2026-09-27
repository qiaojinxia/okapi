-- Acceptance survives the business source transaction (paid order/code/admin).
CREATE TABLE subscription_grants (
    id UUID PRIMARY KEY,
    sequence BIGSERIAL NOT NULL UNIQUE,
    user_id BIGINT NOT NULL REFERENCES users(id),
    source VARCHAR(96) NOT NULL,
    plan_snapshot JSONB NOT NULL,
    actor VARCHAR(64) NOT NULL,
    subscription_id BIGINT REFERENCES user_subscriptions(id),
    outcome VARCHAR(16) CHECK (outcome IN ('activated','renewed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_at TIMESTAMPTZ,
    last_error VARCHAR(64),
    UNIQUE(user_id,source),
    CHECK ((applied_at IS NULL) = (subscription_id IS NULL)),
    CHECK ((applied_at IS NULL) = (outcome IS NULL))
);
CREATE INDEX subscription_grants_pending ON subscription_grants(sequence) WHERE applied_at IS NULL;
-- PG state, financial event and pending repair commit together. The repair uses
-- current authoritative history, retaining every live reservation and hold.
CREATE TABLE subscription_sync (
    user_id BIGINT PRIMARY KEY REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
