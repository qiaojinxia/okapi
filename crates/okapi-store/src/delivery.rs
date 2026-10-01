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
        r#"WITH population AS (
            SELECT CASE WHEN o.status=1 THEN 0 ELSE o.status END AS status,
                   o.created_at,1::bigint AS n,false AS ch
            FROM billing_outbox o WHERE (o.status IN (0,2) OR (o.status=1 AND o.stats_protocol=1))
              AND NOT EXISTS (SELECT 1 FROM billing_ch_events e
                              WHERE e.event_key='outbox:'||o.event_id::text)
            UNION ALL
            SELECT b.status,COALESCE((
                SELECT min(o.created_at) FROM billing_ch_events e JOIN billing_outbox o
                ON o.event_id=CASE WHEN e.event_key LIKE 'outbox:%'
                     THEN substring(e.event_key FROM 8)::uuid END
                WHERE e.batch_id=b.id
            ),b.created_at),b.event_count::bigint,true
            FROM billing_ch_batches b WHERE b.status=0 OR (b.status=2 AND EXISTS (
                SELECT 1 FROM billing_dlq d WHERE d.ch_batch_id=b.id AND d.status=0
            ))
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
