//! Statistics backlog across direct and NATS handoff, counted once per event.
use crate::StoreError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

pub struct Backlog {
    pub pending_events: i64,
    pub failed_events: i64,
    pub ch_pending_events: i64,
    pub oldest_pending_at: Option<DateTime<Utc>>,
}

pub async fn backlog(pg: &PgPool) -> Result<Backlog, StoreError> {
    let r = sqlx::query!(
        // ch_batch_id 只经由 billing_ch_events 连接写入，有值即已入批：先按它过滤才能走部分索引，
        // 否则每次都要把全部历史 outbox 与 events 整表扫一遍做反连接（统计页每次加载都会调）。
        // NOT EXISTS 仍保留，覆盖"已入批、批次未完成、outbox 尚未回写"的窗口。
        // 批次两路分开写：status=0 走 idx_ch_batches_pending，status=2 只看 DLQ 里仍未处理的批次。
        r#"WITH open_batches AS (
            SELECT b.id,b.status,b.created_at,b.event_count FROM billing_ch_batches b WHERE b.status=0
            UNION ALL
            SELECT b.id,b.status,b.created_at,b.event_count FROM billing_ch_batches b
            WHERE b.status=2 AND b.id IN (
                SELECT d.ch_batch_id FROM billing_dlq d WHERE d.status=0 AND d.ch_batch_id IS NOT NULL
            )
        ), population AS (
            SELECT CASE WHEN o.status=1 THEN 0 ELSE o.status END AS status,
                   o.created_at,1::bigint AS n,false AS ch
            FROM billing_outbox o WHERE o.ch_batch_id IS NULL
              AND (o.status IN (0,2) OR (o.status=1 AND o.stats_protocol=1))
              AND NOT EXISTS (SELECT 1 FROM billing_ch_events e
                              WHERE e.event_key='outbox:'||o.event_id::text)
            UNION ALL
            SELECT b.status,COALESCE((
                SELECT min(o.created_at) FROM billing_ch_events e JOIN billing_outbox o
                ON o.event_id=CASE WHEN e.event_key LIKE 'outbox:%'
                     THEN substring(e.event_key FROM 8)::uuid END
                WHERE e.batch_id=b.id
            ),b.created_at),b.event_count::bigint,true
            FROM open_batches b
        ) SELECT COALESCE(sum(n) FILTER (WHERE status=0),0)::bigint AS "pending!",
                 COALESCE(sum(n) FILTER (WHERE status=2),0)::bigint AS "failed!",
                 COALESCE(sum(n) FILTER (WHERE status=0 AND ch),0)::bigint AS "ch_pending!",
                 min(created_at) FILTER (WHERE status=0) AS "oldest?"
          FROM population"#
    )
    .fetch_one(pg)
    .await?;
    Ok(Backlog {
        pending_events: r.pending,
        failed_events: r.failed,
        ch_pending_events: r.ch_pending,
        oldest_pending_at: r.oldest,
    })
}
