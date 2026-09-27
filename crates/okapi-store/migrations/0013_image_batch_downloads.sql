-- Bounded download leases protect sequential archive reads from artifact cleanup.
-- They never authorize submission, settlement or new downloads after expiry/deletion.
CREATE TABLE image_batch_downloads (
    id UUID PRIMARY KEY,
    batch_id UUID NOT NULL REFERENCES image_batches(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX image_batch_downloads_batch ON image_batch_downloads(batch_id,expires_at);
