-- Private pagination checkpoints for finding a previously submitted remote job.
-- A scan never changes money, permits a second POST, or adopts a partial list.
CREATE TABLE image_batch_recovery (
    batch_id UUID PRIMARY KEY REFERENCES image_batches(id) ON DELETE CASCADE,
    next_page TEXT CHECK (octet_length(next_page) BETWEEN 1 AND 4096),
    candidate_name TEXT CHECK (length(candidate_name) BETWEEN 1 AND 1024),
    pages INT NOT NULL DEFAULT 0 CHECK (pages BETWEEN 0 AND 1024),
    cursor_hashes JSONB NOT NULL DEFAULT '[]'
        CHECK (jsonb_typeof(cursor_hashes)='array' AND jsonb_array_length(cursor_hashes)<=1024
            AND octet_length(cursor_hashes::text)<=73728),
    complete BOOLEAN NOT NULL DEFAULT FALSE,
    conflict BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (NOT complete OR next_page IS NULL)
);
