use super::{
    Batch, Error, State, transaction,
    work::{self, Lease},
};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn replay(
    pg: &PgPool,
    uid: i64,
    kid: i64,
    hash: &str,
    request_hash: &str,
) -> Result<Option<Batch>, Error> {
    let row: Option<Batch> = sqlx::query_as(
        "SELECT * FROM image_batches WHERE user_id=$1 AND api_key_id=$2 AND idempotency_hash=$3",
    )
    .bind(uid)
    .bind(kid)
    .bind(hash)
    .fetch_optional(pg)
    .await?;
    if row
        .as_ref()
        .is_some_and(|b| b.request_hash != request_hash || b.delete_requested)
    {
        return Err(Error::IdempotencyConflict);
    }
    Ok(row)
}
#[derive(sqlx::FromRow)]
pub struct Item {
    pub ordinal: i32,
    pub custom_id: String,
    pub prompt_preview: String,
    pub output_count: i32,
    pub outputs: Value,
}
pub struct ItemPage {
    pub data: Vec<Item>,
    pub has_more: bool,
}
pub async fn items_owned(
    pg: &PgPool,
    id: Uuid,
    uid: i64,
    kid: i64,
    after: i32,
    limit: u32,
) -> Result<Option<ItemPage>, Error> {
    if !(-1..200).contains(&after) || !(1..=100).contains(&limit) {
        return Err(Error::Invalid("batch_page"));
    }
    let mut tx = transaction(pg).await?;
    let owner:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM image_batches WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND NOT delete_requested)")
        .bind(id).bind(uid).bind(kid).fetch_one(&mut *tx).await?;
    if !owner {
        return Ok(None);
    }
    let mut data:Vec<Item>=sqlx::query_as("SELECT i.ordinal,i.custom_id,i.prompt_preview,i.output_count,COALESCE((SELECT jsonb_agg(jsonb_build_object('slot',o.slot,'index',o.image_index,'status',o.state,'mime_type',o.content_type,'error_code',o.error_code,'usage',o.usage) ORDER BY o.slot) FROM image_batch_outputs o WHERE o.batch_id=i.batch_id AND o.item_ordinal=i.ordinal),'[]') AS outputs FROM image_batch_items i WHERE i.batch_id=$1 AND i.ordinal>$2 ORDER BY i.ordinal LIMIT $3")
        .bind(id).bind(after).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let has_more = data.len() > limit as usize;
    data.truncate(limit as usize);
    tx.commit().await?;
    Ok(Some(ItemPage { data, has_more }))
}
pub async fn request_delete(
    pg: &PgPool,
    id: Uuid,
    uid: i64,
    kid: i64,
) -> Result<Option<Batch>, Error> {
    let mut tx = transaction(pg).await?;
    let row: Option<Batch> = sqlx::query_as(
        "SELECT * FROM image_batches WHERE id=$1 AND user_id=$2 AND api_key_id=$3 FOR UPDATE",
    )
    .bind(id)
    .bind(uid)
    .bind(kid)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if !row.state.terminal() {
        return Err(Error::Transition);
    }
    let row=sqlx::query_as("UPDATE image_batches SET delete_requested=true,next_run_at=now(),updated_at=now() WHERE id=$1 RETURNING *").bind(id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(row))
}
pub async fn mark_downloaded(pg: &PgPool, id: Uuid, uid: i64, kid: i64) -> Result<(), Error> {
    sqlx::query("UPDATE image_batches SET downloaded_at=COALESCE(downloaded_at,now()) WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND completed_at IS NOT NULL AND NOT delete_requested AND expires_at>clock_timestamp()")
        .bind(id).bind(uid).bind(kid).execute(pg).await?;
    Ok(())
}
pub async fn usage(pg: &PgPool, lease: Lease) -> Result<Vec<Value>, Error> {
    let (mut tx, row) = work::locked(pg, lease).await?;
    if row.state != State::Settling {
        return Err(Error::Transition);
    }
    let values =
        sqlx::query_scalar("SELECT usage FROM image_batch_outputs WHERE batch_id=$1 ORDER BY slot")
            .bind(row.id)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(values)
}
