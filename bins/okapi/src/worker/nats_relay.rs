//! NATS JetStream 传输（docs/database.md §4）：多机形态的事件总线。
//!
//! - relay：billing_outbox（SKIP LOCKED）→ BILLING 流（subject = outbox.topic），
//!   发布确认后标记 published；
//! - chsink（JS 消费者）：durable 拉取 → 批写 CH，dedup_token = JetStream 序号区间
//!   `js-<first_seq>-<last_seq>`（§3.3 精确批次幂等，消除 outbox id 区间的成员漂移边界）；
//!   ack-after-write；投递次数超限 → PG billing_dlq 终态 + ack。
//!
//! 单机无 NATS 时走 chsink::process_once 直连形态（本模块不启用）。

use async_nats::jetstream;
use futures::StreamExt;
use okapi_store::ChClient;
use sqlx::PgPool;
use std::time::Duration;

const STREAM_NAME: &str = "BILLING";
const CONSUMER_NAME: &str = "chsink";
const BATCH_LIMIT: usize = 500;
const MAX_DELIVER: i64 = 5;

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
    let mut tx = pg.begin().await?;
    let rows = sqlx::query!(
        r#"
        SELECT id, topic, created_at, payload
        FROM billing_outbox
        WHERE status = 0 AND (next_retry_at IS NULL OR next_retry_at <= now())
        ORDER BY id
        LIMIT 500
        FOR UPDATE SKIP LOCKED
        "#
    )
    .fetch_all(&mut *tx)
    .await?;
    if rows.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    let mut published: Vec<i64> = Vec::with_capacity(rows.len());
    let mut failed: Vec<i64> = Vec::new();
    for row in &rows {
        // 消费侧需要事件时间：随消息附带 outbox 创建时刻
        let mut payload = row.payload.clone();
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "ts".to_owned(),
                serde_json::Value::String(
                    row.created_at.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                ),
            );
        }
        let ok = match js
            .publish(row.topic.clone(), payload.to_string().into())
            .await
        {
            Ok(ack) => ack.await.is_ok(),
            Err(_) => false,
        };
        if ok {
            published.push(row.id);
        } else {
            failed.push(row.id);
        }
    }

    if !published.is_empty() {
        sqlx::query!(
            r#"UPDATE billing_outbox SET status = 1, published_at = now() WHERE id = ANY($1)"#,
            &published
        )
        .execute(&mut *tx)
        .await?;
    }
    if !failed.is_empty() {
        tracing::warn!(count = failed.len(), "NATS 发布失败，退避重试");
        sqlx::query!(
            r#"
            UPDATE billing_outbox
            SET retry_count = retry_count + 1,
                next_retry_at = now() + make_interval(secs => least(300, 5 * power(2, retry_count)))
            WHERE id = ANY($1)
            "#,
            &failed
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(rows.len())
}

/// chsink（JS 消费者）一批：拉取 → CH 批写（seq 区间 dedup）→ ack。
/// 返回本批消息数。
pub async fn chsink_js_once(
    pg: &PgPool,
    js: &jetstream::Context,
    ch: &ChClient,
) -> anyhow::Result<usize> {
    let stream = js
        .get_stream(STREAM_NAME)
        .await
        .map_err(|e| anyhow::anyhow!("get stream: {e}"))?;
    let consumer: jetstream::consumer::PullConsumer = stream
        .get_or_create_consumer(
            CONSUMER_NAME,
            jetstream::consumer::pull::Config {
                durable_name: Some(CONSUMER_NAME.to_owned()),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ack_wait: Duration::from_secs(30),
                max_deliver: MAX_DELIVER,
                ..Default::default()
            },
        )
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
                tracing::warn!(error = %err, "JS 消息读取失败");
                break;
            }
        }
    }
    if messages.is_empty() {
        return Ok(0);
    }

    let mut rows = Vec::with_capacity(messages.len());
    let mut first_seq = u64::MAX;
    let mut last_seq = 0u64;
    for msg in &messages {
        let seq = msg.info().map_or(0, |i| i.stream_sequence);
        first_seq = first_seq.min(seq);
        last_seq = last_seq.max(seq);
        let payload: serde_json::Value =
            serde_json::from_slice(&msg.payload).unwrap_or(serde_json::Value::Null);
        rows.push(super::chsink::js_payload_to_ch_row(&payload));
    }
    let dedup_token = format!("js-{first_seq}-{last_seq}");

    match ch
        .insert_json_each_row("request_log_raw", &rows, &dedup_token)
        .await
    {
        Ok(()) => {
            for msg in &messages {
                if let Err(err) = msg.ack().await {
                    tracing::warn!(error = %err, "ack 失败（将重投，dedup 兜底）");
                }
            }
        }
        Err(err) => {
            let err_text = err.to_string();
            tracing::warn!(error = %err_text, batch = messages.len(), "chsink(JS) 批写失败");
            // 投递超限的消息转 DLQ 终态并 ack；其余不 ack 等重投
            for msg in &messages {
                let delivered = msg.info().map_or(0, |i| i.delivered);
                if delivered >= MAX_DELIVER {
                    let payload: serde_json::Value =
                        serde_json::from_slice(&msg.payload).unwrap_or(serde_json::Value::Null);
                    sqlx::query!(
                        r#"INSERT INTO billing_dlq (source, payload, error, retry_count) VALUES ('chsink', $1, $2, $3)"#,
                        payload,
                        err_text,
                        i32::try_from(delivered).unwrap_or(i32::MAX)
                    )
                    .execute(pg)
                    .await?;
                    let _ = msg.ack().await;
                }
            }
        }
    }
    Ok(messages.len())
}
