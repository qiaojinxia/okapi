-- Commit identity and immutable CH batches before external delivery.
ALTER TABLE billing_outbox ADD COLUMN event_id UUID NOT NULL DEFAULT gen_random_uuid();
CREATE UNIQUE INDEX idx_outbox_event_id ON billing_outbox (event_id);

CREATE TABLE billing_ch_batches (
    id UUID PRIMARY KEY,
    status SMALLINT NOT NULL DEFAULT 0 CHECK (status IN (0, 1, 2)),
    event_count INTEGER NOT NULL CHECK (event_count BETWEEN 0 AND 500),
    rows JSONB NOT NULL CHECK (jsonb_typeof(rows) = 'array'),
    payloads JSONB NOT NULL CHECK (jsonb_typeof(payloads) = 'array'),
    retry_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CHECK (status = 1 OR (jsonb_array_length(rows) = event_count
                         AND jsonb_array_length(payloads) = event_count))
);
CREATE INDEX idx_ch_batches_pending ON billing_ch_batches (next_retry_at, created_at)
    WHERE status = 0;

CREATE TABLE billing_ch_events (
    event_key TEXT PRIMARY KEY,
    batch_id UUID NOT NULL REFERENCES billing_ch_batches (id)
);
CREATE INDEX idx_ch_events_batch ON billing_ch_events (batch_id);

ALTER TABLE billing_outbox ADD COLUMN ch_batch_id UUID REFERENCES billing_ch_batches (id);
CREATE INDEX idx_outbox_ch_batch ON billing_outbox (ch_batch_id) WHERE ch_batch_id IS NOT NULL;
ALTER TABLE billing_dlq ADD COLUMN ch_batch_id UUID REFERENCES billing_ch_batches (id);
ALTER TABLE billing_dlq ADD COLUMN event_key TEXT;
CREATE UNIQUE INDEX idx_dlq_delivery_event ON billing_dlq (event_key) WHERE event_key IS NOT NULL;
