-- Separate cleanup progress from execution and billing errors. Completed jobs
-- retain their idempotency/financial identity after private artifacts are purged.
CREATE TABLE image_batch_cleanup (
    batch_id UUID PRIMARY KEY REFERENCES image_batches(id) ON DELETE CASCADE,
    job_removed BOOLEAN NOT NULL DEFAULT FALSE,
    operation TEXT CHECK (length(operation) BETWEEN 1 AND 1024),
    last_error VARCHAR(128),
    CHECK (NOT job_removed OR operation IS NULL)
);
CREATE INDEX image_batches_cleanup_due ON image_batches(next_run_at,expires_at,id)
    WHERE completed_at IS NOT NULL AND NOT cleanup_done;
