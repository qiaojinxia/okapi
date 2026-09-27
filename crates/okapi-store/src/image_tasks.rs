//! Durable image task transitions. Every executor mutation is fenced by its lease.
use crate::StoreError;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

pub const MAX_PAYLOAD_BYTES: usize = 48 * 1024 * 1024;
pub const RESULT_BUDGET_BYTES: i64 = 64 * 1024 * 1024;
pub const MAX_RESULT_METADATA_BYTES: usize = 1024 * 1024;
/// Metadata/index allowance, including terminal tasks with no image bytes.
pub const TASK_OVERHEAD_BYTES: i64 = 4096;
const STORAGE_LOCK: i64 = 0x494d_4754_4153_4b53;

/// Shared with reservation expiry: image result+ledger commits own this same row lock.
pub async fn lock_for_balance(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<Transaction<'static, Postgres>>, StoreError> {
    let mut tx = pool.begin().await?;
    if lock_for_balance_in_tx(&mut tx, id).await? {
        Ok(Some(tx))
    } else {
        Ok(None)
    }
}

pub async fn lock_for_balance_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<bool, StoreError> {
    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT t.id FROM image_tasks t JOIN image_task_attempts a ON a.task_id=t.id WHERE a.reservation_id=$1 FOR UPDATE OF t")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(exists.is_some())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Task {
    pub id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub kind: String,
    pub model_name: String,
    pub status: String,
    pub lease_id: Option<Uuid>,
    pub reservation_id: Option<Uuid>,
    pub lease_until: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub cancel_requested: bool,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub http_status: Option<i32>,
    pub billing_pending: bool,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct Claimed {
    #[sqlx(flatten)]
    pub task: Task,
    pub payload: Vec<u8>,
    pub request_hash: String,
    pub client_ip: Option<String>,
    pub client_type: String,
}

pub struct NewTask<'a> {
    pub id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub kind: &'a str,
    pub model: &'a str,
    pub request_hash: &'a str,
    pub idempotency_hash: Option<&'a str>,
    pub payload: &'a [u8],
    pub client_ip: Option<&'a str>,
    pub client_type: &'a str,
}

#[derive(Debug)]
pub enum Enqueued {
    Created(Task),
    Existing(Task),
    Conflict,
    Capacity,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub per_user_bytes: i64,
    pub total_bytes: i64,
    pub per_user_pending: i64,
    pub per_user_tasks: i64,
    pub total_tasks: i64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            per_user_bytes: 256 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            per_user_pending: 8,
            per_user_tasks: 1024,
            total_tasks: 16_384,
        }
    }
}

pub async fn enqueue(
    pool: &PgPool,
    input: NewTask<'_>,
    limits: Limits,
) -> Result<Enqueued, StoreError> {
    if input.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(StoreError::InvalidData("image_task_payload_size"));
    }
    let budget = i64::try_from(input.payload.len())
        .map_err(|_| StoreError::InvalidData("image_task_payload_size"))?
        .checked_add(RESULT_BUDGET_BYTES + TASK_OVERHEAD_BYTES)
        .ok_or(StoreError::InvalidData("image_task_storage_size"))?;
    let mut tx = pool.begin().await?;
    // Serializes admission accounting across users/instances, including idempotent creation.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(STORAGE_LOCK)
        .execute(&mut *tx)
        .await?;
    if let Some(hash) = input.idempotency_hash {
        // Expired, reconciled work no longer owns an idempotency key. Delete only this row;
        // unfinished reconciliation retains its identity until it can be safely cleaned up.
        sqlx::query("DELETE FROM image_tasks t WHERE user_id=$1 AND api_key_id=$2 AND idempotency_hash=$3 AND expires_at<=now() AND status IN ('completed','failed','cancelled') AND NOT billing_pending AND NOT EXISTS(SELECT 1 FROM image_task_objects o WHERE o.task_id=t.id)")
            .bind(input.user_id).bind(input.api_key_id).bind(hash).execute(&mut *tx).await?;
        let existing: Option<Task> = sqlx::query_as("SELECT id,user_id,api_key_id,kind,model_name,status,lease_id,reservation_id,lease_until,attempts,cancel_requested,result,error,http_status,billing_pending,created_at,completed_at,expires_at FROM image_tasks WHERE user_id=$1 AND api_key_id=$2 AND idempotency_hash=$3")
        .bind(input.user_id)
        .bind(input.api_key_id)
        .bind(hash)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(task) = existing {
            let request: (String, String) =
                sqlx::query_as("SELECT request_hash,kind FROM image_tasks WHERE id=$1")
                    .bind(task.id)
                    .fetch_one(&mut *tx)
                    .await?;
            return Ok(
                if request.0 == input.request_hash
                    && request.1 == input.kind
                    && task.expires_at > Utc::now()
                {
                    Enqueued::Existing(task)
                } else {
                    Enqueued::Conflict
                },
            );
        }
    }
    let (total, owned, pending, count, owned_count): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(storage_budget),0)::bigint, COALESCE(SUM(storage_budget) FILTER (WHERE user_id=$1),0)::bigint, COUNT(*) FILTER (WHERE user_id=$1 AND status IN ('queued','preparing','processing')), COUNT(*), COUNT(*) FILTER (WHERE user_id=$1) FROM image_tasks")
        .bind(input.user_id).fetch_one(&mut *tx).await?;
    if budget > limits.total_bytes.saturating_sub(total)
        || budget > limits.per_user_bytes.saturating_sub(owned)
        || pending >= limits.per_user_pending
        || owned_count >= limits.per_user_tasks
        || count >= limits.total_tasks
    {
        return Ok(Enqueued::Capacity);
    }
    let task = sqlx::query_as("INSERT INTO image_tasks(id,user_id,api_key_id,kind,model_name,request_hash,idempotency_hash,payload,client_ip,client_type,storage_budget) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING id,user_id,api_key_id,kind,model_name,status,lease_id,reservation_id,lease_until,attempts,cancel_requested,result,error,http_status,billing_pending,created_at,completed_at,expires_at")
        .bind(input.id).bind(input.user_id).bind(input.api_key_id).bind(input.kind).bind(input.model)
        .bind(input.request_hash).bind(input.idempotency_hash).bind(input.payload).bind(input.client_ip)
        .bind(input.client_type).bind(budget).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Enqueued::Created(task))
}

pub async fn get_owned(
    pool: &PgPool,
    id: Uuid,
    user: i64,
    key: i64,
) -> Result<Option<Task>, StoreError> {
    // Do not fetch request payloads or binary artifacts when polling.
    let task = sqlx::query_as("SELECT id,user_id,api_key_id,kind,model_name,status,lease_id,reservation_id,lease_until,attempts,cancel_requested,result,error,http_status,billing_pending,created_at,completed_at,expires_at FROM image_tasks WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND expires_at>now()")
        .bind(id).bind(user).bind(key).fetch_optional(pool).await?;
    Ok(task)
}

pub async fn claim(pool: &PgPool) -> Result<Option<Claimed>, StoreError> {
    let lease = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    let claimed: Option<Claimed> = sqlx::query_as("UPDATE image_tasks t SET status='preparing',lease_id=$1,reservation_id=$1,lease_until=now()+interval '9 minutes',attempts=attempts+1,updated_at=now(),expires_at=now()+interval '24 hours' WHERE id=(SELECT id FROM image_tasks WHERE status='queued' AND NOT cancel_requested AND expires_at>now() ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING t.*")
        .bind(lease).fetch_optional(&mut *tx).await?;
    if let Some(claimed) = &claimed {
        sqlx::query("INSERT INTO image_task_attempts(reservation_id,task_id) VALUES ($1,$2)")
            .bind(lease)
            .bind(claimed.task.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(claimed)
}

pub async fn dispatch(
    pool: &PgPool,
    id: Uuid,
    lease: Uuid,
    channel: i64,
    key: i64,
) -> Result<bool, StoreError> {
    Ok(sqlx::query("UPDATE image_tasks SET status='processing',channel_id=$3,channel_key_id=$4,updated_at=now() WHERE id=$1 AND lease_id=$2 AND lease_until>now() AND status IN ('preparing','processing') AND NOT cancel_requested")
        .bind(id).bind(lease).bind(channel).bind(key).execute(pool).await?.rows_affected()==1)
}

pub async fn cancel(
    pool: &PgPool,
    id: Uuid,
    user: i64,
    key: i64,
) -> Result<Option<Task>, StoreError> {
    // An in-flight generation cannot be assumed cancelled at the provider. Flag it for the executor.
    Ok(sqlx::query_as("UPDATE image_tasks SET cancel_requested=CASE WHEN status IN ('queued','preparing','processing') THEN true ELSE cancel_requested END,status=CASE WHEN status='queued' THEN 'cancelled' ELSE status END,payload=CASE WHEN status='queued' THEN NULL ELSE payload END,client_ip=CASE WHEN status='queued' THEN NULL ELSE client_ip END,storage_budget=CASE WHEN status='queued' THEN $4 ELSE storage_budget END,completed_at=CASE WHEN status='queued' THEN now() ELSE completed_at END,updated_at=now() WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND expires_at>now() RETURNING id,user_id,api_key_id,kind,model_name,status,lease_id,reservation_id,lease_until,attempts,cancel_requested,result,error,http_status,billing_pending,created_at,completed_at,expires_at")
        .bind(id).bind(user).bind(key).bind(TASK_OVERHEAD_BYTES).fetch_optional(pool).await?)
}

/// Locks and checks a live executor before its caller writes result and settlement in this transaction.
pub async fn lock_live(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    lease: Uuid,
) -> Result<bool, StoreError> {
    Ok(sqlx::query_scalar::<_, bool>("SELECT lease_id=$2 AND lease_until>now() AND status IN ('preparing','processing') FROM image_tasks WHERE id=$1 FOR UPDATE")
        .bind(id).bind(lease).fetch_optional(&mut **tx).await?.unwrap_or(false))
}

pub struct Artifact {
    pub index: i32,
    pub content: Vec<u8>,
    pub content_type: String,
}

/// Must follow `lock_live` and the settlement insert in the same transaction.
pub async fn complete(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    result: &Value,
    status: i32,
    artifacts: &[Artifact],
) -> Result<(), StoreError> {
    let metadata = result.to_string();
    if metadata.len() > MAX_RESULT_METADATA_BYTES {
        return Err(StoreError::InvalidData("image_task_result_metadata_size"));
    }
    let mut bytes = i64::try_from(metadata.len())
        .map_err(|_| StoreError::InvalidData("image_task_result_size"))?;
    for artifact in artifacts {
        bytes = bytes
            .checked_add(
                i64::try_from(artifact.content.len())
                    .map_err(|_| StoreError::InvalidData("image_task_result_size"))?,
            )
            .ok_or(StoreError::InvalidData("image_task_result_size"))?;
        if bytes > RESULT_BUDGET_BYTES {
            return Err(StoreError::InvalidData("image_task_result_size"));
        }
        sqlx::query("INSERT INTO image_task_artifacts(task_id,image_index,content,content_type) VALUES ($1,$2,$3,$4)")
            .bind(id).bind(artifact.index).bind(&artifact.content).bind(&artifact.content_type).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE image_tasks SET status='completed',result=$2,http_status=$3,payload=NULL,client_ip=NULL,storage_budget=$4,completed_at=now(),updated_at=now(),expires_at=now()+interval '24 hours',billing_pending=true,lease_id=NULL,lease_until=NULL WHERE id=$1")
        .bind(id).bind(result).bind(status).bind(bytes + TASK_OVERHEAD_BYTES).execute(&mut **tx).await?;
    Ok(())
}

pub async fn fail(
    pool: &PgPool,
    id: Uuid,
    lease: Uuid,
    status: i32,
    error: &Value,
    cancelled: bool,
) -> Result<bool, StoreError> {
    Ok(sqlx::query("UPDATE image_tasks SET status=$5,error=$3,http_status=$4,payload=NULL,client_ip=NULL,storage_budget=$6,completed_at=now(),updated_at=now(),billing_pending=true,lease_id=NULL,lease_until=NULL WHERE id=$1 AND lease_id=$2 AND status IN ('preparing','processing')")
        .bind(id).bind(lease).bind(error).bind(status).bind(if cancelled {"cancelled"}else{"failed"}).bind(TASK_OVERHEAD_BYTES)
        .execute(pool).await?.rows_affected()==1)
}

#[derive(sqlx::FromRow)]
pub struct StoredArtifact {
    pub content: Option<Vec<u8>>,
    pub content_type: String,
    pub reference: Option<Value>,
    pub content_sha256: Option<String>,
    pub content_bytes: Option<i64>,
}

pub async fn artifact_owned(
    pool: &PgPool,
    id: Uuid,
    index: i32,
    user: i64,
    key: i64,
) -> Result<Option<StoredArtifact>, StoreError> {
    Ok(sqlx::query_as("SELECT a.content,a.content_type,o.reference,o.content_sha256,o.content_bytes FROM image_task_artifacts a JOIN image_tasks t ON t.id=a.task_id LEFT JOIN image_task_objects o ON o.id=a.object_id AND o.state='ready' WHERE t.id=$1 AND a.image_index=$2 AND t.user_id=$3 AND t.api_key_id=$4 AND t.status='completed' AND t.expires_at>now()")
        .bind(id).bind(index).bind(user).bind(key).fetch_optional(pool).await?)
}

/// Terminal rows with unsettled balances must survive until reconciliation has finished.
pub async fn cleanup(pool: &PgPool) -> Result<u64, StoreError> {
    sqlx::query(r#"UPDATE image_tasks SET status='failed',payload=NULL,client_ip=NULL,storage_budget=$1,completed_at=now(),error='{"code":"image_task_expired"}'::jsonb WHERE status='queued' AND expires_at<=now()"#)
        .bind(TASK_OVERHEAD_BYTES).execute(pool).await?;
    Ok(sqlx::query("DELETE FROM image_tasks WHERE id IN (SELECT id FROM image_tasks t WHERE status IN ('completed','failed','cancelled') AND NOT billing_pending AND expires_at<=now() AND NOT EXISTS(SELECT 1 FROM image_task_objects o WHERE o.task_id=t.id) ORDER BY expires_at LIMIT 100 FOR UPDATE SKIP LOCKED)")
        .execute(pool).await?.rows_affected())
}
