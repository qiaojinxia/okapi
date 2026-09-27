-- Native provider jobs outlive gateway requests. Inputs/credentials are kept out
-- of metadata rows so listing jobs never reads prompts, bearer URLs or image bytes.
CREATE TABLE image_batches (
    id UUID PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    api_key_id BIGINT NOT NULL REFERENCES api_keys(id),
    request_hash TEXT NOT NULL CHECK (length(request_hash)=64),
    idempotency_hash TEXT CHECK (length(idempotency_hash)=64),
    task_name VARCHAR(256) NOT NULL,
    parent_id UUID REFERENCES image_batches(id),
    model_name VARCHAR(128) NOT NULL,
    group_code VARCHAR(32) NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('gemini','vertex')),
    channel_id BIGINT NOT NULL REFERENCES channels(id),
    channel_key_id BIGINT NOT NULL REFERENCES channel_keys(id),
    upstream_model VARCHAR(256) NOT NULL,
    pricing_snapshot JSONB NOT NULL CHECK (jsonb_typeof(pricing_snapshot)='object' AND octet_length(pricing_snapshot::text)<=65536),
    unit_quote JSONB NOT NULL CHECK (jsonb_typeof(unit_quote)='object' AND octet_length(unit_quote::text)<=4096),
    maximum_micro BIGINT NOT NULL CHECK (maximum_micro BETWEEN 0 AND 9007199254740991),
    actual_micro BIGINT CHECK (actual_micro BETWEEN 0 AND maximum_micro),
    item_count INT NOT NULL CHECK (item_count BETWEEN 1 AND 200),
    output_count INT NOT NULL CHECK (output_count BETWEEN item_count AND 200),
    success_count INT NOT NULL DEFAULT 0 CHECK (success_count BETWEEN 0 AND output_count),
    failure_count INT NOT NULL DEFAULT 0 CHECK (failure_count BETWEEN 0 AND output_count),
    state TEXT NOT NULL DEFAULT 'funding' CHECK (state IN (
        'funding','preparing','submitting','running','collecting','settling',
        'uncertain','completed','partial','failed','cancelled')),
    cancel_requested BOOLEAN NOT NULL DEFAULT FALSE,
    delete_requested BOOLEAN NOT NULL DEFAULT FALSE,
    cleanup_done BOOLEAN NOT NULL DEFAULT FALSE,
    lease_id UUID,
    lease_until TIMESTAMPTZ,
    next_run_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    submit_intent UUID,
    provider_job_name TEXT CHECK (length(provider_job_name)<=1024),
    remote_state TEXT CHECK (remote_state IN ('pending','running','cancelling','paused','succeeded','partially_succeeded','failed','cancelled','expired')),
    input_ref JSONB NOT NULL DEFAULT '{}',
    output_ref JSONB NOT NULL DEFAULT '{}',
    error_code VARCHAR(128),
    client_ip TEXT,
    client_type VARCHAR(128) NOT NULL,
    storage_budget BIGINT NOT NULL CHECK (storage_budget>=0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    downloaded_at TIMESTAMPTZ,
    UNIQUE(user_id,api_key_id,idempotency_hash),
    CHECK ((lease_id IS NULL)=(lease_until IS NULL)),
    CHECK ((provider_job_name IS NULL)=(remote_state IS NULL)),
    CHECK (provider_job_name IS NULL OR submit_intent IS NOT NULL),
    CHECK ((state IN ('completed','partial','failed','cancelled'))=(completed_at IS NOT NULL)),
    CHECK ((completed_at IS NULL)=(actual_micro IS NULL)),
    CHECK ((completed_at IS NULL)=(expires_at IS NULL)),
    CHECK (success_count+failure_count<=output_count),
    CHECK (octet_length(input_ref::text)<=65536 AND octet_length(output_ref::text)<=65536)
);
CREATE INDEX image_batches_owner ON image_batches(user_id,api_key_id,created_at DESC,id DESC);
CREATE INDEX image_batches_work ON image_batches(next_run_at,created_at,id)
    WHERE completed_at IS NULL;
CREATE INDEX image_batches_cleanup ON image_batches(expires_at,id)
    WHERE completed_at IS NOT NULL;

CREATE TABLE image_batch_payloads (
    batch_id UUID PRIMARY KEY REFERENCES image_batches(id) ON DELETE CASCADE,
    input BYTEA NOT NULL CHECK (octet_length(input) BETWEEN 1 AND 134217728),
    binding BYTEA NOT NULL CHECK (octet_length(binding) BETWEEN 1 AND 262144),
    upload_session BYTEA CHECK (octet_length(upload_session)<=262144)
);

CREATE TABLE image_batch_items (
    batch_id UUID NOT NULL REFERENCES image_batches(id) ON DELETE CASCADE,
    ordinal INT NOT NULL CHECK (ordinal BETWEEN 0 AND 199),
    custom_id VARCHAR(128) NOT NULL,
    prompt_preview VARCHAR(256) NOT NULL,
    output_count INT NOT NULL CHECK (output_count BETWEEN 1 AND 4),
    PRIMARY KEY(batch_id,ordinal),
    UNIQUE(batch_id,custom_id)
);

-- One fixed output slot per requested image. Unknown/duplicate keys cannot create
-- extra billed outputs. Staging is private until the job's hold has settled.
CREATE TABLE image_batch_outputs (
    batch_id UUID NOT NULL,
    slot INT NOT NULL CHECK (slot BETWEEN 0 AND 199),
    item_ordinal INT NOT NULL,
    image_index INT NOT NULL CHECK (image_index BETWEEN 0 AND 3),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','succeeded','failed')),
    content BYTEA CHECK (octet_length(content) BETWEEN 1 AND 16777216),
    content_type TEXT CHECK (content_type IN ('image/png','image/jpeg','image/webp')),
    content_hash TEXT CHECK (length(content_hash)=64),
    usage JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(usage)='object' AND octet_length(usage::text)<=8192),
    error_code VARCHAR(128),
    PRIMARY KEY(batch_id,slot),
    UNIQUE(batch_id,item_ordinal,image_index),
    FOREIGN KEY(batch_id,item_ordinal) REFERENCES image_batch_items(batch_id,ordinal) ON DELETE CASCADE,
    CHECK ((state='succeeded')=(content IS NOT NULL)),
    CHECK ((state='succeeded')=(content_type IS NOT NULL)),
    CHECK ((state='succeeded')=(content_hash IS NOT NULL)),
    CHECK ((state='failed')=(error_code IS NOT NULL))
);
