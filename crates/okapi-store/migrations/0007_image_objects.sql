-- Durable offload intent exists before any S3 write. Task expiry cannot discard it.
CREATE TABLE image_task_objects (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES image_tasks(id),
    image_index INT NOT NULL CHECK (image_index BETWEEN 0 AND 9),
    reference JSONB NOT NULL CHECK (octet_length(reference::text) <= 8192),
    content_sha256 TEXT NOT NULL CHECK (length(content_sha256) = 64),
    content_bytes BIGINT NOT NULL CHECK (content_bytes BETWEEN 1 AND 67108864),
    state TEXT NOT NULL CHECK (state IN ('pending', 'ready', 'deleting')),
    lease_id UUID,
    lease_until TIMESTAMPTZ,
    retry_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts INT NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(task_id, image_index),
    CHECK ((lease_id IS NULL) = (lease_until IS NULL))
);
CREATE INDEX image_task_objects_work ON image_task_objects(retry_at, lease_until);

ALTER TABLE image_task_artifacts ALTER COLUMN content DROP NOT NULL;
ALTER TABLE image_task_artifacts ADD COLUMN object_id UUID REFERENCES image_task_objects(id);
ALTER TABLE image_task_artifacts ADD CONSTRAINT image_artifact_source
    CHECK (content IS NOT NULL OR object_id IS NOT NULL);
