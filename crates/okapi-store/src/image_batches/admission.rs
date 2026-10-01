use super::Error;
use super::{
    Batch, Created, JOB_OVERHEAD, Limits, MAX_IMAGE_BYTES, MAX_INPUT_BYTES, NewBatch,
    OVERHEAD_PER_OUTPUT, STORAGE_LOCK, transaction,
};
use sqlx::PgPool;
use std::collections::BTreeSet;

fn bounded(value: &str, max: usize, empty: bool) -> bool {
    (empty || !value.trim().is_empty())
        && value.len() <= max
        && !value.chars().any(char::is_control)
}
fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn validate(input: &NewBatch<'_>) -> Result<(i32, i64), Error> {
    if input.id.is_nil()
        || input.user_id <= 0
        || input.api_key_id <= 0
        || input.channel_id <= 0
        || input.channel_key_id <= 0
        || !digest(input.request_hash)
        || input.idempotency_hash.is_some_and(|v| !digest(v))
        || !bounded(input.model, 128, false)
        || !bounded(input.group, 32, false)
        || !bounded(input.upstream_model, 256, false)
        || !bounded(input.task_name, 256, true)
        || !bounded(input.client_type, 128, true)
        || !matches!(input.provider, "gemini" | "vertex")
        || input.input.is_empty()
        || input.input.len() > MAX_INPUT_BYTES
        || input.binding.is_empty()
        || input.binding.len() > 262_144
        || input.items.is_empty()
        || input.items.len() > 200
        || !input.pricing.is_object()
        || input.pricing.to_string().len() > 65_536
        || !input.unit_quote.is_object()
        || input.unit_quote.to_string().len() > 4096
        || !(0..=9_007_199_254_740_991).contains(&input.maximum.as_micros())
    {
        return Err(Error::Invalid("batch_admission"));
    }
    let mut seen = BTreeSet::new();
    let mut outputs = 0_i32;
    for item in input.items {
        if !bounded(item.custom_id, 128, false)
            || !bounded(item.prompt_preview, 256, true)
            || !(1..=4).contains(&item.outputs)
            || !seen.insert(item.custom_id)
        {
            return Err(Error::Invalid("batch_items"));
        }
        outputs = outputs
            .checked_add(i32::try_from(item.outputs).map_err(|_| Error::Invalid("batch_outputs"))?)
            .ok_or(Error::Invalid("batch_outputs"))?;
    }
    if outputs > 200 {
        return Err(Error::Invalid("batch_outputs"));
    }
    let total = super::UnitQuote::read(input.unit_quote)?
        .total(u32::try_from(outputs).map_err(|_| Error::Invalid("batch_outputs"))?)?;
    if total.amount != input.maximum.as_micros()
        || input
            .pricing
            .get("mode")
            .and_then(serde_json::Value::as_str)
            != Some("per_call")
        || input
            .pricing
            .get("group")
            .and_then(serde_json::Value::as_str)
            != Some(input.group)
        || input
            .pricing
            .get("epoch")
            .and_then(serde_json::Value::as_i64)
            .is_none()
        || input
            .pricing
            .get("media_units")
            .and_then(serde_json::Value::as_i64)
            != Some(i64::from(outputs))
    {
        return Err(Error::Invalid("batch_quote"));
    }
    let image_budget = i64::try_from(MAX_IMAGE_BYTES)
        .map_err(|_| Error::Invalid("batch_budget"))?
        .checked_add(OVERHEAD_PER_OUTPUT)
        .and_then(|n| n.checked_mul(i64::from(outputs)))
        .ok_or(Error::Invalid("batch_budget"))?;
    let budget = i64::try_from(input.input.len())
        .ok()
        .and_then(|n| n.checked_add(image_budget))
        .and_then(|n| n.checked_add(JOB_OVERHEAD))
        .ok_or(Error::Invalid("batch_budget"))?;
    Ok((outputs, budget))
}

pub async fn create(pg: &PgPool, input: NewBatch<'_>, limits: Limits) -> Result<Created, Error> {
    create_admitted(pg, input, limits, || async { Ok(()) }).await
}

/// Run the external rate gate only for a new, valid task, while the existing
/// storage lock serializes idempotent submissions. The gate must be bounded,
/// must not acquire another PG connection, and must not call the provider.
/// A successful gate followed by an unknown/failed PG commit can consume an
/// attempt without creating a task; never undo shared rate counts speculatively.
pub async fn create_admitted<F, Fut>(
    pg: &PgPool,
    input: NewBatch<'_>,
    limits: Limits,
    admit: F,
) -> Result<Created, Error>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), Error>>,
{
    let (outputs, budget) = validate(&input)?;
    let mut tx = transaction(pg).await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(STORAGE_LOCK)
        .execute(&mut *tx)
        .await?;
    if let Some(hash) = input.idempotency_hash {
        let old:Option<Batch>=sqlx::query_as("SELECT * FROM image_batches WHERE user_id=$1 AND api_key_id=$2 AND idempotency_hash=$3")
            .bind(input.user_id).bind(input.api_key_id).bind(hash).fetch_optional(&mut *tx).await?;
        if let Some(old) = old {
            if old.request_hash != input.request_hash || old.delete_requested {
                return Err(Error::IdempotencyConflict);
            }
            tx.commit().await?;
            return Ok(Created::Existing(old));
        }
    }
    let permitted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys k JOIN users u ON u.id=k.user_id WHERE k.id=$1 AND k.user_id=$2 AND k.status=1 AND k.deleted_at IS NULL AND (k.expires_at IS NULL OR k.expires_at>now()) AND u.status=1 AND u.deleted_at IS NULL) AND EXISTS(SELECT 1 FROM channel_keys ck JOIN channels c ON c.id=ck.channel_id WHERE ck.id=$3 AND c.id=$4 AND ck.status=1 AND c.status=1 AND c.deleted_at IS NULL AND c.provider=$5)")
        .bind(input.api_key_id).bind(input.user_id).bind(input.channel_key_id).bind(input.channel_id).bind(input.provider).fetch_one(&mut *tx).await?;
    if !permitted {
        return Err(Error::AdmissionChanged);
    }
    // Persist the financial intent atomically with the job. A key disabled immediately
    // afterwards must still be cancellable without leaving an uncloseable funding row.
    sqlx::query("SELECT pg_advisory_xact_lock($1,hashtext($2))")
        .bind(super::HOLD_LOCK_NAMESPACE)
        .bind(input.user_id.to_string())
        .execute(&mut *tx)
        .await?;
    let holds: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM balance_holds WHERE user_id=$1 AND state<>'closed'",
    )
    .bind(input.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if holds >= super::MAX_ACTIVE_HOLDS {
        return Err(Error::Capacity);
    }
    let (mode,quota,spent,member):(i16,Option<i64>,i64,Option<i64>)=sqlx::query_as("SELECT quota_mode,quota_micro,used_micro,member_user_id FROM api_keys WHERE id=$1 AND user_id=$2 FOR UPDATE")
        .bind(input.api_key_id).bind(input.user_id).fetch_one(&mut *tx).await?;
    let outstanding:i64=sqlx::query_scalar("SELECT COALESCE(SUM(maximum_micro),0)::bigint FROM balance_holds WHERE api_key_id=$1 AND state<>'closed'")
        .bind(input.api_key_id).fetch_one(&mut *tx).await?;
    if mode == 1
        && spent
            .checked_add(outstanding)
            .and_then(|n| n.checked_add(input.maximum.as_micros()))
            .is_none_or(|n| n > quota.unwrap_or(0))
    {
        return Err(Error::Budget);
    }
    if let Some(parent) = input.parent_id {
        let parent_owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM image_batches WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND NOT delete_requested)")
            .bind(parent).bind(input.user_id).bind(input.api_key_id).fetch_one(&mut *tx).await?;
        if !parent_owned {
            return Err(Error::ParentOwner);
        }
    }
    let (total,user,key_active,user_active,bytes,user_bytes):(i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT COUNT(*) FILTER(WHERE NOT cleanup_done),COUNT(*) FILTER(WHERE user_id=$1 AND NOT cleanup_done),COUNT(*) FILTER(WHERE user_id=$1 AND api_key_id=$2 AND completed_at IS NULL),COUNT(*) FILTER(WHERE user_id=$1 AND completed_at IS NULL),COALESCE(SUM(storage_budget),0)::bigint,COALESCE(SUM(storage_budget) FILTER(WHERE user_id=$1),0)::bigint FROM image_batches WHERE NOT cleanup_done")
        .bind(input.user_id).bind(input.api_key_id).fetch_one(&mut *tx).await?;
    if total >= limits.total_jobs
        || user >= limits.per_user_jobs
        || key_active >= limits.per_key_active
        || user_active >= limits.per_user_active
        || bytes
            .checked_add(budget)
            .is_none_or(|n| n > limits.total_bytes)
        || user_bytes
            .checked_add(budget)
            .is_none_or(|n| n > limits.per_user_bytes)
    {
        return Err(Error::Capacity);
    }
    let item_count = i32::try_from(input.items.len()).map_err(|_| Error::Invalid("batch_items"))?;
    admit().await?;
    let row:Batch=sqlx::query_as("INSERT INTO image_batches(id,user_id,api_key_id,request_hash,idempotency_hash,task_name,parent_id,model_name,group_code,provider,channel_id,channel_key_id,upstream_model,pricing_snapshot,unit_quote,maximum_micro,item_count,output_count,client_ip,client_type,storage_budget,member_user_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22) RETURNING *")
        .bind(input.id).bind(input.user_id).bind(input.api_key_id).bind(input.request_hash).bind(input.idempotency_hash).bind(input.task_name).bind(input.parent_id)
        .bind(input.model).bind(input.group).bind(input.provider).bind(input.channel_id).bind(input.channel_key_id).bind(input.upstream_model)
        .bind(input.pricing).bind(input.unit_quote).bind(input.maximum.as_micros()).bind(item_count).bind(outputs).bind(input.client_ip).bind(input.client_type).bind(budget).bind(member)
        .fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO balance_holds(id,user_id,api_key_id,model_name,request_hash,maximum_micro,pricing_snapshot) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(input.id).bind(input.user_id).bind(input.api_key_id).bind(input.model).bind(input.request_hash).bind(input.maximum.as_micros()).bind(input.pricing)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO image_batch_payloads(batch_id,input,binding) VALUES($1,$2,$3)")
        .bind(input.id)
        .bind(input.input)
        .bind(input.binding)
        .execute(&mut *tx)
        .await?;
    insert_items(&mut tx, input.id, input.items).await?;
    tx.commit().await?;
    Ok(Created::New(row))
}

async fn insert_items(
    conn: &mut sqlx::PgConnection,
    batch_id: uuid::Uuid,
    items: &[super::NewItem<'_>],
) -> Result<(), Error> {
    let mut slot = 0_i32;
    for (ordinal, item) in items.iter().enumerate() {
        let ordinal = i32::try_from(ordinal).map_err(|_| Error::Invalid("batch_items"))?;
        let count = i32::try_from(item.outputs).map_err(|_| Error::Invalid("batch_outputs"))?;
        sqlx::query("INSERT INTO image_batch_items(batch_id,ordinal,custom_id,prompt_preview,output_count) VALUES($1,$2,$3,$4,$5)")
            .bind(batch_id).bind(ordinal).bind(item.custom_id).bind(item.prompt_preview).bind(count).execute(&mut *conn).await?;
        for image in 0..count {
            sqlx::query("INSERT INTO image_batch_outputs(batch_id,slot,item_ordinal,image_index) VALUES($1,$2,$3,$4)").bind(batch_id).bind(slot).bind(ordinal).bind(image).execute(&mut *conn).await?;
            slot = slot.checked_add(1).ok_or(Error::Invalid("batch_outputs"))?;
        }
    }
    Ok(())
}
