-- Recoverable copies are optional: historical hashes cannot be reversed.
-- Only AES-256-GCM envelopes may be written here; never plaintext.
ALTER TABLE api_keys ADD COLUMN key_ciphertext BYTEA;
COMMENT ON COLUMN api_keys.key_ciphertext IS
    'Optional encrypted API key for owner-session copy; NULL for legacy/hash-only keys';
