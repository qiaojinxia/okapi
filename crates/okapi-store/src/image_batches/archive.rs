use super::{Batch, Error, MAX_IMAGE_BYTES, transaction};
use sqlx::PgPool;
use uuid::Uuid;

pub const LEASE_SECONDS: u64 = 600;
#[derive(Clone, Copy)]
pub struct Lease {
    batch: Uuid,
    token: Uuid,
}
#[derive(sqlx::FromRow)]
pub struct Entry {
    pub slot: i32,
    pub image_index: i32,
    pub custom_id: String,
    pub state: String,
    pub content_type: Option<String>,
    pub content_hash: Option<String>,
    pub bytes: Option<i32>,
    pub error_code: Option<String>,
}
pub struct Archive {
    pub batch: Batch,
    pub entries: Vec<Entry>,
    pub lease: Option<Lease>,
}
/// HEAD requests read metadata only. GET starts a bounded lease and records the
/// download attempt atomically with publication/ownership validation.
pub async fn open(
    pg: &PgPool,
    id: Uuid,
    uid: i64,
    kid: i64,
    download: bool,
) -> Result<Option<Archive>, Error> {
    let mut tx = transaction(pg).await?;
    let batch: Option<Batch> = sqlx::query_as(
        "SELECT * FROM image_batches WHERE id=$1 AND user_id=$2 AND api_key_id=$3 FOR UPDATE",
    )
    .bind(id)
    .bind(uid)
    .bind(kid)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut batch) = batch else {
        return Ok(None);
    };
    let eligible:bool=sqlx::query_scalar("SELECT $1 AND NOT $2 AND NOT $3 AND $4::timestamptz>clock_timestamp() AND ($5::timestamptz IS NULL OR $5<=clock_timestamp())")
        .bind(batch.state.terminal()).bind(batch.delete_requested).bind(batch.cleanup_done).bind(batch.expires_at).bind(batch.lease_until).fetch_one(&mut *tx).await?;
    if !eligible || batch.success_count == 0 {
        return Ok(None);
    }
    let entries:Vec<Entry>=sqlx::query_as("SELECT o.slot,o.image_index,i.custom_id,o.state,o.content_type,o.content_hash,octet_length(o.content) AS bytes,o.error_code FROM image_batch_outputs o JOIN image_batch_items i ON i.batch_id=o.batch_id AND i.ordinal=o.item_ordinal WHERE o.batch_id=$1 ORDER BY o.slot")
        .bind(id).fetch_all(&mut *tx).await?;
    if entries.len() != usize::try_from(batch.output_count).map_err(|_| Error::ResultsIncomplete)?
        || entries.len() > 200
        || entries.iter().any(|e| e.state == "pending")
        || entries.iter().filter(|e| e.state == "succeeded").count()
            != usize::try_from(batch.success_count).map_err(|_| Error::ResultsIncomplete)?
        || entries.iter().any(|e| {
            e.state == "succeeded"
                && e.bytes.is_none_or(|n| {
                    n <= 0 || usize::try_from(n).map_or(true, |n| n > MAX_IMAGE_BYTES)
                })
        })
    {
        return Err(Error::ResultsIncomplete);
    }
    let lease = if download {
        sqlx::query(
            "DELETE FROM image_batch_downloads WHERE batch_id=$1 AND expires_at<=clock_timestamp()",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let active: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM image_batch_downloads WHERE batch_id=$1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if active >= 16 {
            return Err(Error::Capacity);
        }
        let lease = Lease {
            batch: id,
            token: Uuid::new_v4(),
        };
        sqlx::query("INSERT INTO image_batch_downloads(id,batch_id,expires_at) VALUES($1,$2,clock_timestamp()+($3::bigint * interval '1 second'))")
            .bind(lease.token).bind(id).bind(i64::try_from(LEASE_SECONDS).map_err(|_|Error::Invalid("batch_download_lease"))?).execute(&mut *tx).await?;
        batch.downloaded_at=Some(sqlx::query_scalar(
            "UPDATE image_batches SET downloaded_at=COALESCE(downloaded_at,now()) WHERE id=$1 RETURNING downloaded_at",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?);
        Some(lease)
    } else {
        None
    };
    tx.commit().await?;
    Ok(Some(Archive {
        batch,
        entries,
        lease,
    }))
}
/// A previously authorized download may finish after user deletion or TTL expiry.
/// The opaque, unexpired lease is its authority; fresh downloads are still denied.
pub async fn content(pg: &PgPool, lease: Lease, slot: i32) -> Result<Option<Vec<u8>>, Error> {
    Ok(sqlx::query_scalar("SELECT o.content FROM image_batch_outputs o JOIN image_batch_downloads d ON d.batch_id=o.batch_id WHERE d.id=$1 AND d.batch_id=$2 AND d.expires_at>clock_timestamp() AND o.slot=$3 AND o.state='succeeded'")
        .bind(lease.token).bind(lease.batch).bind(slot).fetch_optional(pg).await?)
}
pub async fn release(pg: &PgPool, lease: Lease) -> Result<(), Error> {
    sqlx::query("DELETE FROM image_batch_downloads WHERE id=$1 AND batch_id=$2")
        .bind(lease.token)
        .bind(lease.batch)
        .execute(pg)
        .await?;
    Ok(())
}
