//! Leased terminal-only cleanup. No financial mutations are allowed here.
use super::{Batch, Error, JOB_OVERHEAD, Payload, transaction};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct Lease {
    id: Uuid,
    token: Uuid,
}
pub struct Claim {
    pub batch: Batch,
    pub lease: Lease,
    pub progress: Progress,
}
#[derive(sqlx::FromRow)]
pub struct Progress {
    pub job_removed: bool,
    pub operation: Option<String>,
}
async fn locked(
    pg: &PgPool,
    lease: Lease,
) -> Result<(Transaction<'static, Postgres>, Batch), Error> {
    let mut tx = transaction(pg).await?;
    let row: Batch = sqlx::query_as("SELECT * FROM image_batches WHERE id=$1 AND lease_id=$2 AND completed_at IS NOT NULL AND NOT cleanup_done FOR UPDATE")
        .bind(lease.id).bind(lease.token).fetch_optional(&mut *tx).await?.ok_or(Error::LeaseLost)?;
    let eligible: bool = sqlx::query_scalar(
        "SELECT $1::timestamptz>clock_timestamp() AND ($2 OR $3::timestamptz<=clock_timestamp())",
    )
    .bind(row.lease_until)
    .bind(row.delete_requested)
    .bind(row.expires_at)
    .fetch_one(&mut *tx)
    .await?;
    if !eligible {
        return Err(Error::LeaseLost);
    }
    settled(&mut tx, &row).await?;
    Ok((tx, row))
}
async fn settled(tx: &mut Transaction<'_, Postgres>, row: &Batch) -> Result<(), Error> {
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM balance_holds WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND request_hash=$4 AND maximum_micro=$5 AND pricing_snapshot=$6 AND state='closed' AND actual_micro=$7 AND model_name=$8)")
        .bind(row.id).bind(row.user_id).bind(row.api_key_id).bind(&row.request_hash).bind(row.maximum_micro).bind(&row.pricing_snapshot).bind(row.actual_micro).bind(&row.model_name).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(Error::NotSettled);
    }
    Ok(())
}
pub async fn claim(pg: &PgPool, only: Option<Uuid>) -> Result<Option<Claim>, Error> {
    let mut tx = transaction(pg).await?;
    let row: Option<Batch> = sqlx::query_as("SELECT * FROM image_batches WHERE completed_at IS NOT NULL AND NOT cleanup_done AND (delete_requested OR expires_at<=clock_timestamp()) AND NOT EXISTS(SELECT 1 FROM image_batch_downloads d WHERE d.batch_id=image_batches.id AND d.expires_at>clock_timestamp()) AND next_run_at<=clock_timestamp() AND (lease_until IS NULL OR lease_until<=clock_timestamp()) AND ($1::uuid IS NULL OR id=$1) ORDER BY next_run_at,expires_at,id FOR UPDATE SKIP LOCKED LIMIT 1")
        .bind(only).fetch_optional(&mut *tx).await?;
    let Some(row) = row else { return Ok(None) };
    let downloading:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM image_batch_downloads WHERE batch_id=$1 AND expires_at>clock_timestamp())").bind(row.id).fetch_one(&mut *tx).await?;
    if downloading {
        return Ok(None);
    }
    settled(&mut tx, &row).await?;
    let lease = Lease {
        id: row.id,
        token: Uuid::new_v4(),
    };
    let batch=sqlx::query_as("UPDATE image_batches SET lease_id=$2,lease_until=clock_timestamp()+interval '120 seconds' WHERE id=$1 RETURNING *")
        .bind(row.id).bind(lease.token).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO image_batch_cleanup(batch_id) VALUES($1) ON CONFLICT DO NOTHING")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    let progress =
        sqlx::query_as("SELECT job_removed,operation FROM image_batch_cleanup WHERE batch_id=$1")
            .bind(row.id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(Some(Claim {
        batch,
        lease,
        progress,
    }))
}
pub async fn payload(pg: &PgPool, lease: Lease) -> Result<Payload, Error> {
    let (mut tx, _) = locked(pg, lease).await?;
    let payload = sqlx::query_as(
        "SELECT input,binding,upload_session FROM image_batch_payloads WHERE batch_id=$1",
    )
    .bind(lease.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(payload)
}
pub async fn renew(pg: &PgPool, lease: Lease) -> Result<(), Error> {
    let (mut tx, _) = locked(pg, lease).await?;
    sqlx::query(
        "UPDATE image_batches SET lease_until=clock_timestamp()+interval '120 seconds' WHERE id=$1",
    )
    .bind(lease.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
pub async fn release(
    pg: &PgPool,
    lease: Lease,
    seconds: u32,
    error: Option<&str>,
) -> Result<(), Error> {
    if seconds > 3600 || error.is_some_and(|e| !super::work::error_code(e)) {
        return Err(Error::Invalid("batch_cleanup_retry"));
    }
    let (mut tx, _) = locked(pg, lease).await?;
    sqlx::query("UPDATE image_batch_cleanup SET last_error=$2 WHERE batch_id=$1")
        .bind(lease.id)
        .bind(error)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE image_batches SET lease_id=NULL,lease_until=NULL,next_run_at=clock_timestamp()+($2::bigint * interval '1 second') WHERE id=$1").bind(lease.id).bind(i64::from(seconds)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
/// An operation reference may be forgotten after a failed/missing LRO; this never
/// proves that a job was removed. Only the executor's fresh absence check does.
pub async fn operation(pg: &PgPool, lease: Lease, name: Option<&str>) -> Result<(), Error> {
    if name.is_some_and(|n| n.is_empty() || n.len() > 1024 || n.chars().any(char::is_control)) {
        return Err(Error::Invalid("batch_cleanup_operation"));
    }
    let (mut tx, _) = locked(pg, lease).await?;
    let changed = sqlx::query(
        "UPDATE image_batch_cleanup SET operation=$2 WHERE batch_id=$1 AND NOT job_removed",
    )
    .bind(lease.id)
    .bind(name)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(Error::Transition);
    }
    tx.commit().await?;
    Ok(())
}
pub async fn job_removed(pg: &PgPool, lease: Lease) -> Result<(), Error> {
    let (mut tx, _) = locked(pg, lease).await?;
    sqlx::query("UPDATE image_batch_cleanup SET job_removed=true,operation=NULL WHERE batch_id=$1")
        .bind(lease.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
/// Called only after remote files have been confirmed absent. Metadata keeps a
/// conservative storage reservation; cleanup cannot erase idempotency or bills.
pub async fn finish(pg: &PgPool, lease: Lease) -> Result<(), Error> {
    let (mut tx, _) = locked(pg, lease).await?;
    let ready: bool =
        sqlx::query_scalar("SELECT job_removed FROM image_batch_cleanup WHERE batch_id=$1")
            .bind(lease.id)
            .fetch_one(&mut *tx)
            .await?;
    if !ready {
        return Err(Error::Transition);
    }
    for query in [
        "DELETE FROM image_batch_downloads WHERE batch_id=$1",
        "DELETE FROM image_batch_payloads WHERE batch_id=$1",
        "DELETE FROM image_batch_items WHERE batch_id=$1",
        "DELETE FROM image_batch_recovery WHERE batch_id=$1",
        "DELETE FROM image_batch_cleanup WHERE batch_id=$1",
    ] {
        sqlx::query(query).bind(lease.id).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE image_batches SET cleanup_done=true,storage_budget=$2,input_ref='{}',output_ref='{}',lease_id=NULL,lease_until=NULL,updated_at=now() WHERE id=$1").bind(lease.id).bind(JOB_OVERHEAD).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
