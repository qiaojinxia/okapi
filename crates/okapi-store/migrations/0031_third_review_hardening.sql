-- Consume successful authenticator counters atomically across login/confirmation.
ALTER TABLE users ADD COLUMN totp_last_counter BIGINT;
CREATE INDEX image_batches_live_capacity_idx ON image_batches(user_id,api_key_id)
    INCLUDE(storage_budget,completed_at) WHERE NOT cleanup_done;
