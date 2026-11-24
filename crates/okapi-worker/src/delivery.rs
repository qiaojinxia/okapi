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
    // 只有统计类型进 CH：别的 log_type 落进 request_log_raw 会让该统计桶永久判为历史不完整。
    // 事件照常认领、批次照常完成（outbox 不会反复重投），只是不写 CH。
    let mut rows = Vec::with_capacity(events.len());
    let mut payloads = Vec::with_capacity(events.len());
    for event in &events {
        match event.row.get("log_type").and_then(Value::as_i64) {
            Some(kind) if okapi_store::ch::STATISTICS_LOG_TYPES.contains(&kind) => {
                rows.push(event.row.clone());
                payloads.push(json!({"key":event.key,"payload":event.payload}));
            }
            kind => tracing::warn!(
                event = %event.key,
                log_type = ?kind,
                "billing event type is not a statistics type; not written to ClickHouse"
            ),
        }
    }
    // 批次表约束 rows / payloads 条数都等于 event_count：被拦下的事件两边都不进
    let count = i32::try_from(rows.len())?;
    let rows = Value::Array(rows);
    let payloads = Value::Array(payloads);
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

#[derive(sqlx::FromRow)]
struct ClaimedBatch {
    id: Uuid,
    event_count: i32,
    rows: Value,
    next_retry_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Deliver exactly the saved rows, even when fresh events arrive or a prior PG commit failed.
///
/// 采用两阶段短租约（与 `nats_relay::relay_once` 同口径）：先在 <1ms 事务里写入 5 分钟租约并立即释放
/// PG 连接与行锁，再执行 ClickHouse HTTP 写入，最后用短事务按 CAS 落盘结果；避免 CH 慢请求期间把
/// PG 连接钉死在 `idle in transaction` 并阻塞 `VACUUM`。
pub(super) async fn deliver_once(pg: &PgPool, ch: &ChClient) -> anyhow::Result<usize> {
    let claimed = sqlx::query_as::<_, ClaimedBatch>(
        "UPDATE billing_ch_batches b
         SET next_retry_at = now() + interval '5 minutes'
         FROM (
             SELECT id FROM billing_ch_batches
             WHERE status = 0 AND (next_retry_at IS NULL OR next_retry_at <= now())
             ORDER BY created_at, id
             LIMIT 1
             FOR UPDATE SKIP LOCKED
         ) c
         WHERE b.id = c.id
         RETURNING b.id, b.event_count, b.rows, b.next_retry_at",
    )
    .fetch_optional(pg)
    .await?;
    let Some(batch) = claimed else {
        return Ok(0);
    };
    let lease_at = batch
        .next_retry_at
        .context("claimed delivery batch must carry a lease timestamp")?;
    let rows = batch
        .rows
        .as_array()
        .context("stored delivery rows must be an array")?;
    anyhow::ensure!(
        rows.len() == usize::try_from(batch.event_count)?,
        "stored delivery size mismatch"
    );
    let token = format!("billing-batch-v1-{}", batch.id);
    // ClickHouse HTTP I/O runs with zero PostgreSQL transaction/connection held.
    let insert_result = ch
        .insert_json_each_row("request_log_raw", rows, &token)
        .await;
    let finalize = match insert_result {
        Ok(()) => complete_batch(pg, batch.id).await,
        Err(err) => record_failure(pg, batch.id, lease_at, &err.to_string()).await,
    };
    if let Err(err) = finalize {
        // 进程仍存活但回写 PG 失败（如瞬态冲突/故障注入）：主动释放租约以便下一拍立即重试，不必空等 5 分钟。
        let _ = sqlx::query(
            "UPDATE billing_ch_batches
             SET next_retry_at = NULL
             WHERE id = $1 AND status = 0 AND next_retry_at = $2",
        )
        .bind(batch.id)
        .bind(lease_at)
        .execute(pg)
        .await;
        return Err(err);
    }
    Ok(usize::try_from(batch.event_count)?)
}

async fn complete_batch(pg: &PgPool, id: Uuid) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    // Compaction is atomic with completion; rollback restores the exact frozen rows.
    let completed = sqlx::query_scalar::<_, Uuid>(
        "UPDATE billing_ch_batches
         SET status = 1, completed_at = now(), next_retry_at = NULL, rows = '[]', payloads = '[]'
         WHERE id = $1 AND status = 0
         RETURNING id",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if completed.is_some() {
        sqlx::query(
            "UPDATE billing_outbox o
             SET status = 1, published_at = COALESCE(o.published_at, now()),
                 ch_batch_id = $1, next_retry_at = NULL
             FROM billing_ch_events e
             WHERE e.batch_id = $1
               AND o.event_id = CASE WHEN e.event_key LIKE 'outbox:%'
                   THEN substring(e.event_key FROM 8)::uuid END",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn record_failure(
    pg: &PgPool,
    id: Uuid,
    lease_at: chrono::DateTime<chrono::Utc>,
    error: &str,
) -> anyhow::Result<()> {
    tracing::warn!(batch_id=%id, error, "CH frozen batch failed; retry original batch");
    let mut tx = pg.begin().await?;
    // CAS 校验租约归属：若租约已过期并被其他副本接管或完成，放弃写入以免重复累加 retry_count。
    let updated = sqlx::query_scalar::<_, Uuid>(
        "UPDATE billing_ch_batches
         SET retry_count = retry_count + 1,
             next_retry_at = now() + make_interval(secs => least(300, 5 * power(2, retry_count))),
             status = CASE WHEN retry_count + 1 >= $2 THEN 2 ELSE 0 END
         WHERE id = $1 AND status = 0 AND next_retry_at = $3
         RETURNING id",
    )
    .bind(id)
    .bind(MAX_RETRY)
    .bind(lease_at)
    .fetch_optional(&mut *tx)
    .await?;
    if updated.is_none() {
        tx.commit().await?;
        return Ok(());
    }
    sqlx::query(
        "UPDATE billing_outbox o
         SET status = CASE WHEN b.status = 2 THEN 2 ELSE o.status END,
             retry_count = b.retry_count,
             next_retry_at = b.next_retry_at,
             ch_batch_id = b.id
         FROM billing_ch_events e
         JOIN billing_ch_batches b ON b.id = e.batch_id
         WHERE b.id = $1
           AND o.event_id = CASE WHEN e.event_key LIKE 'outbox:%'
               THEN substring(e.event_key FROM 8)::uuid END",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO billing_dlq (source, payload, error, retry_count, ch_batch_id, event_key)
         SELECT 'chsink', v->'payload', $2, b.retry_count, b.id, v->>'key'
         FROM billing_ch_batches b
         CROSS JOIN LATERAL jsonb_array_elements(b.payloads) v
         WHERE b.id = $1 AND b.status = 2
         ON CONFLICT (event_key) WHERE event_key IS NOT NULL DO NOTHING",
    )
    .bind(id)
    .bind(error)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
