//! Both transports hand immutable batches to PG before CH I/O.
//! Completed identity receipts outlive outbox retention and JS redelivery.
use anyhow::Context;
use okapi_store::ChClient;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) const BATCH_LIMIT: i64 = 500;
const MAX_RETRY: i32 = 5;

pub(super) struct Event {
    pub key: String,
    pub payload: Value,
    pub row: Value,
}

pub(super) fn outbox_key(id: Uuid) -> String {
    format!("outbox:{id}")
}

/// Caller commits this transaction before any CH write or NATS ack.
/// Unique event keys serialize competing admissions; sorted insertion avoids lock inversion.
pub(super) async fn admit(tx: &mut PgConnection, mut events: Vec<Event>) -> anyhow::Result<()> {
    anyhow::ensure!(
        events.len() <= 500,
        "delivery admission exceeds batch limit"
    );
    if events.is_empty() {
        return Ok(());
    }
    events.sort_by(|a, b| a.key.cmp(&b.key));
    events.dedup_by(|a, b| a.key == b.key);
    let id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO billing_ch_batches (id,event_count,rows,payloads) VALUES ($1,0,'[]','[]')",
        id
    )
    .execute(&mut *tx)
    .await?;
    let keys: Vec<String> = events.iter().map(|e| e.key.clone()).collect();
    let claimed = sqlx::query_scalar!(
        r#"INSERT INTO billing_ch_events (event_key,batch_id)
           SELECT k,$2 FROM unnest($1::text[]) AS k ORDER BY k
           ON CONFLICT (event_key) DO NOTHING RETURNING event_key"#,
        &keys,
        id
    )
    .fetch_all(&mut *tx)
    .await?;
    if claimed.is_empty() {
        sqlx::query!("DELETE FROM billing_ch_batches WHERE id=$1", id)
            .execute(&mut *tx)
            .await?;
        return Ok(());
    }
    let claimed: HashSet<String> = claimed.into_iter().collect();
    let events: Vec<Event> = events
        .into_iter()
        .filter(|e| claimed.contains(&e.key))
        .collect();
    let count = i32::try_from(events.len())?;
    let rows = Value::Array(events.iter().map(|e| e.row.clone()).collect());
    let payloads = Value::Array(
        events
            .iter()
            .map(|e| json!({"key":e.key,"payload":e.payload}))
            .collect(),
    );
    sqlx::query!(
        "UPDATE billing_ch_batches SET event_count=$2,rows=$3,payloads=$4 WHERE id=$1",
        id,
        count,
        rows,
        payloads
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Deliver exactly the saved rows, even when fresh events arrive or a prior PG commit failed.
pub(super) async fn deliver_once(pg: &PgPool, ch: &ChClient) -> anyhow::Result<usize> {
    let mut tx = pg.begin().await?;
    let batch = sqlx::query!(
        r#"SELECT id,event_count,rows FROM billing_ch_batches
           WHERE status=0 AND (next_retry_at IS NULL OR next_retry_at<=now())
           ORDER BY created_at,id LIMIT 1 FOR UPDATE SKIP LOCKED"#
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(batch) = batch else {
        tx.commit().await?;
        return Ok(0);
    };
    let rows = batch
        .rows
        .as_array()
        .context("stored delivery rows must be an array")?;
    anyhow::ensure!(
        rows.len() == usize::try_from(batch.event_count)?,
        "stored delivery size mismatch"
    );
    let token = format!("billing-batch-v1-{}", batch.id);
    match ch
        .insert_json_each_row("request_log_raw", rows, &token)
        .await
    {
        Ok(()) => {
            // Compaction is atomic with completion; rollback restores the exact frozen rows.
            sqlx::query!(
                r#"UPDATE billing_ch_batches SET status=1,completed_at=now(),next_retry_at=NULL,
                   rows='[]',payloads='[]' WHERE id=$1"#,
                batch.id
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                r#"UPDATE billing_outbox o SET status=1,published_at=COALESCE(o.published_at,now()),
                   ch_batch_id=$1,next_retry_at=NULL
                   FROM billing_ch_events e
                   WHERE e.batch_id=$1 AND o.event_id=CASE WHEN e.event_key LIKE 'outbox:%'
                       THEN substring(e.event_key FROM 8)::uuid END"#,
                batch.id
            )
            .execute(&mut *tx)
            .await?;
        }
        Err(err) => {
            record_failure(&mut tx, batch.id, &err.to_string()).await?;
        }
    }
    tx.commit().await?;
    Ok(usize::try_from(batch.event_count)?)
}

async fn record_failure(tx: &mut PgConnection, id: Uuid, error: &str) -> anyhow::Result<()> {
    tracing::warn!(batch_id=%id, error, "CH frozen batch failed; retry original batch");
    sqlx::query!(
        r#"UPDATE billing_ch_batches
           SET retry_count=retry_count+1,
               next_retry_at=now()+make_interval(secs=>least(300,5*power(2,retry_count))),
               status=CASE WHEN retry_count+1 >= $2 THEN 2 ELSE 0 END WHERE id=$1"#,
        id,
        MAX_RETRY
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"UPDATE billing_outbox o SET status=CASE WHEN b.status=2 THEN 2 ELSE o.status END,
           retry_count=b.retry_count,next_retry_at=b.next_retry_at,ch_batch_id=b.id
           FROM billing_ch_events e JOIN billing_ch_batches b ON b.id=e.batch_id
           WHERE b.id=$1 AND o.event_id=CASE WHEN e.event_key LIKE 'outbox:%'
               THEN substring(e.event_key FROM 8)::uuid END"#,
        id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO billing_dlq (source,payload,error,retry_count,ch_batch_id,event_key)
           SELECT 'chsink',v->'payload',$2,b.retry_count,b.id,v->>'key'
           FROM billing_ch_batches b CROSS JOIN LATERAL jsonb_array_elements(b.payloads) v
           WHERE b.id=$1 AND b.status=2
           ON CONFLICT (event_key) WHERE event_key IS NOT NULL DO NOTHING"#,
        id,
        error
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}
