use okapi_providers::{
    aws_sigv4::payload_hash,
    image_store::{ObjectRef, S3Store},
};
use okapi_store::StoreError;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub(super) struct Work {
    pub id: Uuid,
    pub lease: Uuid,
    pub reference: ObjectRef,
    pub content: Option<Vec<u8>>,
    pub mime: String,
    pub hash: String,
    pub deleting: bool,
}

pub(super) async fn claim(
    pool: &PgPool,
    active: Option<&S3Store>,
) -> Result<Option<Work>, StoreError> {
    let mut tx = pool.begin().await?;
    let lease = Uuid::new_v4();
    // Existing intents are serviced even after new offloads have been disabled.
    let existing:Option<(Uuid,Value,String,bool)>=sqlx::query_as("SELECT o.id,o.reference,o.content_sha256,(o.state='deleting' OR t.expires_at<=now()+CASE WHEN o.state='pending' THEN interval '3 minutes' ELSE interval '0 seconds' END) FROM image_task_objects o JOIN image_tasks t ON t.id=o.task_id WHERE (o.lease_until IS NULL OR o.lease_until<=now()) AND o.retry_at<=now() AND (o.state IN ('pending','deleting') OR (t.expires_at<=now() AND NOT t.billing_pending)) ORDER BY o.retry_at,o.id LIMIT 1 FOR UPDATE OF o SKIP LOCKED")
        .fetch_optional(&mut *tx).await?;
    if let Some((id, reference, hash, deleting)) = existing {
        let source: Option<(Option<Vec<u8>>, String)> = if deleting {
            None
        } else {
            sqlx::query_as("SELECT a.content,a.content_type FROM image_task_artifacts a JOIN image_task_objects o ON o.task_id=a.task_id AND o.image_index=a.image_index WHERE o.id=$1")
            .bind(id).fetch_optional(&mut *tx).await?
        };
        sqlx::query("UPDATE image_task_objects SET state=CASE WHEN $3 THEN 'deleting' ELSE 'pending' END,lease_id=$2,lease_until=now()+interval '3 minutes',attempts=attempts+1,updated_at=now() WHERE id=$1")
            .bind(id).bind(lease).bind(deleting).execute(&mut *tx).await?;
        let reference = serde_json::from_value(reference)
            .map_err(|_| StoreError::InvalidData("image_object_reference"))?;
        tx.commit().await?;
        let (content, mime) = source.unwrap_or_default();
        return Ok(Some(Work {
            id,
            lease,
            reference,
            content,
            mime,
            hash,
            deleting,
        }));
    }
    let Some(store) = active else {
        return Ok(None);
    };
    // Older result handling could persist empty base64 alongside a nonempty URL.
    // Leave those PG-only until expiry; they must not poison the upload queue.
    let source:Option<(Uuid,i32,Vec<u8>,String)>=sqlx::query_as("SELECT a.task_id,a.image_index,a.content,a.content_type FROM image_task_artifacts a JOIN image_tasks t ON t.id=a.task_id WHERE t.status='completed' AND t.expires_at>now()+interval '3 minutes' AND octet_length(a.content)>0 AND NOT EXISTS(SELECT 1 FROM image_task_objects o WHERE o.task_id=a.task_id AND o.image_index=a.image_index) ORDER BY t.created_at,a.image_index LIMIT 1 FOR UPDATE OF a SKIP LOCKED")
        .fetch_optional(&mut *tx).await?;
    let Some((task, index, content, mime)) = source else {
        return Ok(None);
    };
    let id = Uuid::new_v4();
    let reference = store.object(format!("okapi-images/{}/{id}.bin", task.simple()));
    let hash = payload_hash(&content);
    let value = serde_json::to_value(&reference)
        .map_err(|_| StoreError::InvalidData("image_object_reference"))?;
    sqlx::query("INSERT INTO image_task_objects(id,task_id,image_index,reference,content_sha256,content_bytes,state,lease_id,lease_until,attempts) VALUES($1,$2,$3,$4,$5,$6,'pending',$7,now()+interval '3 minutes',1)")
        .bind(id).bind(task).bind(index).bind(value).bind(&hash).bind(i64::try_from(content.len()).map_err(|_|StoreError::InvalidData("image_object_size"))?).bind(lease).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(Work {
        id,
        lease,
        reference,
        content: Some(content),
        mime,
        hash,
        deleting: false,
    }))
}

pub(super) async fn finish(
    pool: &PgPool,
    work: &Work,
    reference: Option<ObjectRef>,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    let owned:Option<Uuid>=sqlx::query_scalar("SELECT id FROM image_task_objects WHERE id=$1 AND lease_id=$2 AND lease_until>now() FOR UPDATE")
        .bind(work.id).bind(work.lease).fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Ok(());
    }
    if let Some(reference) = reference {
        let reference = serde_json::to_value(reference)
            .map_err(|_| StoreError::InvalidData("image_object_reference"))?;
        sqlx::query("UPDATE image_task_objects SET reference=$2,state='ready',lease_id=NULL,lease_until=NULL,last_error=NULL,updated_at=now() WHERE id=$1")
            .bind(work.id).bind(reference).execute(&mut *tx).await?;
        sqlx::query("UPDATE image_task_artifacts a SET object_id=$1,content=NULL FROM image_task_objects o WHERE o.id=$1 AND a.task_id=o.task_id AND a.image_index=o.image_index")
            .bind(work.id).execute(&mut *tx).await?;
    } else {
        // Only expired ready objects point to this row. Pending uploads retain their PG source.
        sqlx::query("DELETE FROM image_task_artifacts WHERE object_id=$1")
            .bind(work.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM image_task_objects WHERE id=$1")
            .bind(work.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(super) async fn retry(pool: &PgPool, work: &Work, code: &str) -> Result<(), StoreError> {
    sqlx::query("UPDATE image_task_objects SET lease_id=NULL,lease_until=NULL,retry_at=now()+interval '30 seconds',last_error=$3,updated_at=now() WHERE id=$1 AND lease_id=$2")
        .bind(work.id).bind(work.lease).bind(code).execute(pool).await?;
    Ok(())
}
