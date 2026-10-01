-- Distinguish new relay handoff from historical published rows lacking receipts.
ALTER TABLE billing_outbox ADD COLUMN stats_protocol SMALLINT NOT NULL DEFAULT 0
    CHECK (stats_protocol IN (0,1));
CREATE INDEX idx_outbox_published_unassigned ON billing_outbox (id)
    WHERE status=1 AND stats_protocol=1 AND ch_batch_id IS NULL;
