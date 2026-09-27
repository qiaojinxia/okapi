-- Durable image requests. Redis contains balances, never image payloads/results.
CREATE TABLE image_tasks (
    id UUID PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    api_key_id BIGINT NOT NULL REFERENCES api_keys(id),
    kind TEXT NOT NULL CHECK (kind IN ('generation', 'edit')),
    model_name TEXT NOT NULL,
    request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
    idempotency_hash TEXT CHECK (length(idempotency_hash) = 64),
    payload BYTEA,
    client_ip TEXT,
    client_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'preparing', 'processing', 'completed', 'failed', 'cancelled')),
    lease_id UUID,
    reservation_id UUID,
    lease_until TIMESTAMPTZ,
    attempts INT NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    cancel_requested BOOLEAN NOT NULL DEFAULT false,
    channel_id BIGINT,
    channel_key_id BIGINT,
    result JSONB,
    error JSONB,
    http_status INT,
    billing_pending BOOLEAN NOT NULL DEFAULT false,
    storage_budget BIGINT NOT NULL CHECK (storage_budget >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '24 hours',
    UNIQUE (user_id, api_key_id, idempotency_hash),
    CHECK (payload IS NULL OR octet_length(payload) <= 50331648),
    CHECK (result IS NULL OR octet_length(result::text) <= 1048576),
    CHECK ((lease_id IS NULL) = (lease_until IS NULL))
);
CREATE INDEX image_tasks_queue ON image_tasks(created_at, id) WHERE status = 'queued';
CREATE INDEX image_tasks_recovery ON image_tasks(lease_until)
    WHERE status IN ('preparing', 'processing');
CREATE INDEX image_tasks_owner ON image_tasks(user_id, api_key_id, created_at DESC);
CREATE INDEX image_tasks_expiry ON image_tasks(expires_at);
CREATE INDEX image_tasks_billing ON image_tasks(updated_at) WHERE billing_pending;

-- Each execution attempt uses a distinct reservation. A stale worker can never
-- overwrite a newer attempt's reservation when it resumes after losing its lease.
CREATE TABLE image_task_attempts (
    reservation_id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES image_tasks(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX image_task_attempts_task ON image_task_attempts(task_id);

CREATE TABLE image_task_artifacts (
    task_id UUID NOT NULL REFERENCES image_tasks(id) ON DELETE CASCADE,
    image_index INT NOT NULL CHECK (image_index BETWEEN 0 AND 9),
    content BYTEA NOT NULL CHECK (octet_length(content) <= 67108864),
    content_type TEXT NOT NULL,
    PRIMARY KEY(task_id, image_index)
);
