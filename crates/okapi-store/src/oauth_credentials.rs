//! OAuth maintenance queries and compare-and-swap writes. Never serialize credential bytes.
use crate::{StoreError, credential};
use serde_json::Value;
use sqlx::PgPool;

#[derive(sqlx::FromRow)]
pub struct KeyRow {
    pub id: i64,
    pub channel_id: i64,
    pub provider: String,
    pub settings: Value,
    pub status: i16,
    pub credential_ciphertext: Vec<u8>,
}

pub struct Snapshot {
    pub stored: Vec<u8>,
    pub plaintext: String,
    pub status: i16,
}

/// Account maintenance providers come from registered hooks, independently of
/// credential kind. A new quota-capable API adapter needs no storage query changes.
pub async fn scan(
    pool: &PgPool,
    after: i64,
    limit: i64,
    providers: &[&str],
) -> Result<Vec<KeyRow>, StoreError> {
    Ok(sqlx::query_as::<_, KeyRow>(
        r"SELECT k.id, k.channel_id, c.provider, c.settings, k.status, k.credential_ciphertext
           FROM channel_keys k JOIN channels c ON c.id = k.channel_id
           WHERE k.id > $1 AND k.status IN (1,2,3)
             AND c.status = 1 AND c.deleted_at IS NULL
             AND c.provider = ANY($3)
           ORDER BY k.id LIMIT $2",
    )
    .bind(after)
    .bind(limit)
    .bind(providers)
    .fetch_all(pool)
    .await?)
}

pub async fn target(pool: &PgPool, channel: i64, key: i64) -> Result<Option<KeyRow>, StoreError> {
    Ok(sqlx::query_as!(
        KeyRow,
        r#"SELECT k.id, k.channel_id, c.provider, c.settings, k.status, k.credential_ciphertext
           FROM channel_keys k JOIN channels c ON c.id = k.channel_id
           WHERE k.id = $2 AND k.channel_id = $1 AND k.credential_kind = 1
             AND c.deleted_at IS NULL AND c.provider IN ('anthropic_max','codex')"#,
        channel,
        key
    )
    .fetch_optional(pool)
    .await?)
}

pub async fn snapshot(
    pool: &PgPool,
    key: i64,
    master: Option<&str>,
) -> Result<Option<Snapshot>, StoreError> {
    let row = sqlx::query!(
        "SELECT credential_ciphertext, status FROM channel_keys WHERE id = $1",
        key
    )
    .fetch_optional(pool)
    .await?;
    row.map(|r| {
        Ok(Snapshot {
            plaintext: credential::open(master, &r.credential_ciphertext)?,
            stored: r.credential_ciphertext,
            status: r.status,
        })
    })
    .transpose()
}

/// Refresh preserves scheduling status and cannot overwrite a concurrently replaced credential.
pub async fn write_if_current(
    pool: &PgPool,
    key: i64,
    provider: &str,
    expected: &[u8],
    plaintext: &str,
    master: Option<&str>,
) -> Result<bool, StoreError> {
    let result = sqlx::query!(
        r#"UPDATE channel_keys k SET credential_ciphertext = $4, updated_at = now()
           WHERE k.id = $1 AND k.credential_ciphertext = $3 AND k.status IN (1,2,3)
             AND EXISTS (SELECT 1 FROM channels c WHERE c.id = k.channel_id
                         AND c.provider = $2 AND c.deleted_at IS NULL)"#,
        key,
        provider,
        expected,
        credential::seal_or_plain(master, plaintext)?
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn invalidate_if_current(
    pool: &PgPool,
    key: i64,
    provider: &str,
    expected: &[u8],
    error: &str,
) -> Result<bool, StoreError> {
    let result = sqlx::query!(
        r#"UPDATE channel_keys k SET status = 6, cooldown_until = NULL,
              last_error = $4, failed_count = failed_count + 1, updated_at = now()
           WHERE k.id = $1 AND k.credential_ciphertext = $3 AND k.status IN (1,2,3)
             AND EXISTS (SELECT 1 FROM channels c WHERE c.id = k.channel_id
                         AND c.provider = $2 AND c.deleted_at IS NULL)"#,
        key,
        provider,
        expected,
        error
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn reauthorize(
    pool: &PgPool,
    channel: i64,
    key: i64,
    provider: &str,
    expected: &[u8],
    plaintext: &str,
    master: Option<&str>,
) -> Result<bool, StoreError> {
    let result = sqlx::query!(
        r#"UPDATE channel_keys k SET credential_ciphertext = $4, status = 1,
              cooldown_until = NULL, failed_count = 0, last_error = NULL, updated_at = now()
           WHERE id = $2 AND channel_id = $1 AND credential_kind = 1
             AND credential_ciphertext = $3
             AND EXISTS (SELECT 1 FROM channels c WHERE c.id = k.channel_id
                         AND c.provider = $5 AND c.deleted_at IS NULL)"#,
        channel,
        key,
        expected,
        credential::seal_or_plain(master, plaintext)?,
        provider
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}
