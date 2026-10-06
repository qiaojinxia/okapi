//! NATS JetStream 传输（docs/database.md §4）：多机形态的事件总线。
//!
//! - relay：billing_outbox（SKIP LOCKED）→ BILLING 流（subject = outbox.topic），
//!   发布确认后标记 published；
//! - chsink：durable 拉取 → PG 持久接管身份/冻结批次 → ack → CH；
//!   重投与直连形态共用回执，CH 重试及 DLQ 由 PG 批次负责。
//!
//! 单机无 NATS 时走 chsink::process_once 直连形态（本模块不启用）。

use async_nats::jetstream;
use futures::StreamExt;
use okapi_store::ChClient;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

use super::delivery::{self, Event};

const STREAM_NAME: &str = "BILLING";
const CONSUMER_NAME: &str = "chsink";
const BATCH_LIMIT: usize = 500;

/// 确保 BILLING 流与 chsink 消费者存在（幂等）。
pub async fn ensure_topology(client: &async_nats::Client) -> anyhow::Result<jetstream::Context> {
    let js = jetstream::new(client.clone());
    js.get_or_create_stream(jetstream::stream::Config {
        name: STREAM_NAME.to_owned(),
        subjects: vec!["billing.>".to_owned()],
        retention: jetstream::stream::RetentionPolicy::Limits,
        max_age: Duration::from_hours(48),
        storage: jetstream::stream::StorageType::File,
        num_replicas: 1, // 生产 R=3（docs/database.md §4.1）
        ..Default::default()
    })
    .await
    .map_err(|e| anyhow::anyhow!("ensure stream: {e}"))?;
    Ok(js)
}

/// relay 一批：outbox pending → JetStream 发布（确认后标记 published）。
/// 返回本批行数。发布失败走 outbox 既有退避列。
pub async fn relay_once(pg: &PgPool, js: &jetstream::Context) -> anyhow::Result<usize> {
    // Claim with a 10-minute lease, then release PG locks before network I/O.
    let rows=sqlx::query!(r#"UPDATE billing_outbox o SET next_retry_at=now()+interval '10 minutes'
        FROM (SELECT id FROM billing_outbox WHERE status=0 AND ch_batch_id IS NULL
              AND (next_retry_at IS NULL OR next_retry_at<=now())
              AND NOT EXISTS(SELECT 1 FROM billing_ch_events e WHERE e.event_key='outbox:'||billing_outbox.event_id::text)
              ORDER BY id LIMIT 500 FOR UPDATE SKIP LOCKED) c
        WHERE o.id=c.id RETURNING o.id,o.event_id,o.topic,o.created_at,o.payload,o.retry_count,o.next_retry_at"#).fetch_all(pg).await?;
    let count = rows.len();
    let outcomes = futures::stream::iter(rows)
        .map(|row| async move {
            let mut payload = row.payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("_billing_event_id".into(), serde_json::json!(row.event_id));
                obj.insert(
                    "ts".into(),
                    serde_json::json!(row.created_at.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
                );
            }
            let valid = row.topic.starts_with("billing.")
                && !row
                    .topic
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || matches!(b, b'*' | b'>'))
                && parse_event(
                    payload.to_string().as_bytes(),
                    &delivery::outbox_key(row.event_id),
                )
                .is_ok();
            let ok = valid
                && tokio::time::timeout(Duration::from_secs(5), async {
                    match js
                        .send_publish(
                            row.topic.clone(),
                            jetstream::message::PublishMessage::build()
                                .message_id(delivery::outbox_key(row.event_id))
                                .payload(payload.to_string().into()),
                        )
                        .await
                    {
                        Ok(ack) => ack.await.is_ok(),
                        Err(_) => false,
                    }
                })
                .await
                .unwrap_or(false);
            (row, ok, valid)
        })
        .buffer_unordered(32)
        .collect::<Vec<_>>()
        .await;
    for (row, ok, valid) in outcomes {
        let mut tx = pg.begin().await?;
        // Recheck the lease: late acknowledgements cannot alter another worker's claim.
        let owned=sqlx::query_scalar!("SELECT id FROM billing_outbox WHERE id=$1 AND status=0 AND ch_batch_id IS NULL AND next_retry_at=$2 FOR UPDATE",row.id,row.next_retry_at).fetch_optional(&mut *tx).await?;
        if owned.is_none() {
            tx.commit().await?;
            continue;
        }
        if ok {
            sqlx::query!("UPDATE billing_outbox SET status=1,published_at=now(),stats_protocol=1,next_retry_at=NULL WHERE id=$1",row.id).execute(&mut *tx).await?;
        } else if !valid || row.retry_count >= 4 {
            let key = delivery::outbox_key(row.event_id);
            let error = if valid {
                "relay_publish_retries_exhausted"
            } else {
                "relay_invalid_event"
            };
            sqlx::query!(r#"INSERT INTO billing_dlq(source,payload,error,retry_count,event_key)
                VALUES('relay',$1,$2,$3,$4) ON CONFLICT(event_key) WHERE event_key IS NOT NULL DO NOTHING"#,row.payload,error,row.retry_count.saturating_add(1),key).execute(&mut *tx).await?;
            sqlx::query!("UPDATE billing_outbox SET status=2,retry_count=retry_count+1,next_retry_at=NULL WHERE id=$1",row.id).execute(&mut *tx).await?;
        } else {
            sqlx::query!("UPDATE billing_outbox SET retry_count=retry_count+1,next_retry_at=now()+make_interval(secs=>least(300,5*power(2,least(retry_count,6)))) WHERE id=$1",row.id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    Ok(count)
}

/// PG takes durable responsibility before ack; saved batches survive empty JS fetches.
pub async fn chsink_js_once(
    pg: &PgPool,
    js: &jetstream::Context,
    ch: &ChClient,
) -> anyhow::Result<usize> {
    // Resume pending direct/JS batches even if NATS is temporarily unavailable.
    super::chsink::recover_published(pg).await?;
    let delivered = delivery::deliver_once(pg, ch).await?;
    let stream = js
        .get_stream(STREAM_NAME)
        .await
        .map_err(|e| anyhow::anyhow!("get stream: {e}"))?;
    // Create-or-update preserves the durable cursor while upgrading old max_deliver=5.
    // A PG outage must not exhaust delivery attempts before durable handoff.
    let consumer: jetstream::consumer::PullConsumer = stream
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some(CONSUMER_NAME.to_owned()),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ack_wait: Duration::from_secs(30),
            max_deliver: -1,
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!("ensure consumer: {e}"))?;
    let mut batch = consumer
        .fetch()
        .max_messages(BATCH_LIMIT)
        .expires(Duration::from_secs(1))
        .messages()
        .await
        .map_err(|e| anyhow::anyhow!("fetch: {e}"))?;
    let mut messages = Vec::new();
    while let Some(item) = batch.next().await {
        match item {
            Ok(msg) => messages.push(msg),
            Err(err) => {
                tracing::warn!(error=%err, "JS fetch interrupted");
                break;
            }
        }
    }
    if messages.is_empty() {
        return Ok(delivered);
    }
    let mut tx = pg.begin().await?;
    let mut events = Vec::with_capacity(messages.len());
    for msg in &messages {
        let info = msg
            .info()
            .map_err(|e| anyhow::anyhow!("JS message identity: {e}"))?;
        let legacy_key = format!(
            "js:{}:{}",
            stream.cached_info().created,
            info.stream_sequence
        );
        match parse_event(&msg.payload, &legacy_key) {
            Ok(event) => events.push(event),
            Err(err) => {
                let payload =
                    serde_json::json!({"invalid_message":String::from_utf8_lossy(&msg.payload)});
                sqlx::query!(
                    r#"INSERT INTO billing_dlq (source,payload,error,retry_count,event_key)
                       VALUES ('jetstream',$1,$2,0,$3)
                       ON CONFLICT (event_key) WHERE event_key IS NOT NULL DO NOTHING"#,
                    payload,
                    err.to_string(),
                    legacy_key
                )
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    delivery::admit(&mut tx, events).await?;
    tx.commit().await?;
    acknowledge_with_budget(
        messages
            .iter()
            .cloned()
            .map(|message| async move { message.double_ack().await }),
        Duration::from_secs(10),
    )
    .await;
    delivery::deliver_once(pg, ch).await?;
    Ok(messages.len())
}

async fn acknowledge_with_budget<I, F, E>(acknowledgements: I, budget: Duration)
where
    I: IntoIterator<Item = F> + Send,
    I::IntoIter: Send,
    F: std::future::Future<Output = Result<(), E>> + Send,
    E: std::fmt::Display + Send,
{
    // PG owns durable replay responsibility before this phase. Canceling a
    // slow acknowledgement is safe and must not stall CH delivery for minutes.
    let work = futures::stream::iter(acknowledgements).for_each_concurrent(32, |ack| async move {
        match tokio::time::timeout(Duration::from_secs(5), ack).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "JS ack failed; PG receipt handles replay"),
            Err(_) => tracing::warn!("JS ack timed out; PG receipt handles replay"),
        }
    });
    if tokio::time::timeout(budget, work).await.is_err() {
        tracing::warn!("JS acknowledgement batch deadline reached; PG receipt handles replay");
    }
}

fn parse_event(data: &[u8], legacy_key: &str) -> anyhow::Result<Event> {
    let payload: serde_json::Value = serde_json::from_slice(data)?;
    anyhow::ensure!(payload.is_object(), "JS billing payload must be an object");
    let request_id = payload
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("JS billing request_id missing"))?;
    Uuid::parse_str(request_id)?;
    anyhow::ensure!(
        payload
            .get("log_type")
            .and_then(serde_json::Value::as_i64)
            .is_some(),
        "JS billing log_type missing"
    );
    anyhow::ensure!(
        payload
            .get("ts")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "JS billing event timestamp missing"
    );
    let key = match payload.get("_billing_event_id") {
        None => legacy_key.to_owned(),
        Some(value) => delivery::outbox_key(Uuid::parse_str(
            value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("JS billing event identity invalid"))?,
        )?),
    };
    let row = super::chsink::js_payload_to_ch_row(&payload);
    Ok(Event { key, payload, row })
}

#[cfg(test)]
mod acknowledgement_tests {
    #[tokio::test]
    async fn stalled_acknowledgements_have_a_batch_deadline() {
        let started = std::time::Instant::now();
        super::acknowledge_with_budget(
            (0..500).map(|_| std::future::pending::<Result<(), std::io::Error>>()),
            std::time::Duration::from_millis(30),
        )
        .await;
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}
