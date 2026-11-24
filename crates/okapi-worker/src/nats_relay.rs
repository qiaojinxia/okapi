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
pub(super) const BATCH_LIMIT: usize = 500;

/// 缓存的 JetStream 拉取消费者句柄及流创建时间，避免每秒 tick 重复发送 `get_stream` + `create_consumer` RPC。
#[derive(Clone)]
pub(super) struct CachedConsumer {
    consumer: jetstream::consumer::PullConsumer,
    stream_created: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct JsBatchReport {
    pub(super) delivered: usize,
    pub(super) fetched: usize,
}

/// 创建或更新持久化 `chsink` 消费者并缓存其元数据；NATS 异常时调用方可丢弃缓存重新调用以自愈。
pub(super) async fn ensure_consumer(js: &jetstream::Context) -> anyhow::Result<CachedConsumer> {
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
    let stream_created = stream.cached_info().created.to_string();
    Ok(CachedConsumer {
        consumer,
        stream_created,
    })
}

/// 从 outbox payload 的 `diagnostics` 提取 W3C `traceparent`，接力写入 NATS 消息头。
fn traceparent_from_payload(payload: &serde_json::Value) -> Option<String> {
    let diag = payload.get("diagnostics")?;
    let trace_id = diag.get("trace_id").and_then(serde_json::Value::as_str)?;
    let span_id = diag.get("span_id").and_then(serde_json::Value::as_str)?;
    let candidate = format!("00-{trace_id}-{span_id}-01");
    okapi_kernel::trace_context::parse_traceparent(&candidate).map(|_| candidate)
}

/// 确保 BILLING 流与 chsink 消费者存在（幂等）。
pub async fn ensure_topology(client: &async_nats::Client) -> anyhow::Result<jetstream::Context> {
    let js = jetstream::new(client.clone());
    // 副本数与容量上限可配，且每次启动按配置更新已有的流（以前只在首次创建时生效，
    // 「生产 R=3」写在注释里永远不会落地）。容量满了丢最旧的：未消费的事件在 PG outbox
    // 里仍有已发布记录，recover_published 会重新认领，不丢账。
    let replicas =
        okapi_store::env_config::number("OKAPI_NATS_REPLICAS", 1, |n| (1..=5).contains(n));
    let max_bytes =
        okapi_store::env_config::number("OKAPI_NATS_MAX_BYTES", 4 * 1024 * 1024 * 1024, |n| {
            // 下限防误配成几 KB 把流挤爆；上限 1 TiB 防把「GB」当成「字节」多写几个 0，
            // 声明的容量远超磁盘时 JetStream 不会拒，满盘时才一起出事
            (64 * 1024 * 1024..=1 << 40).contains(n)
        });
    js.create_or_update_stream(jetstream::stream::Config {
        name: STREAM_NAME.to_owned(),
        subjects: vec!["billing.>".to_owned()],
        retention: jetstream::stream::RetentionPolicy::Limits,
        discard: jetstream::stream::DiscardPolicy::Old,
        max_age: Duration::from_hours(48),
        max_bytes,
        storage: jetstream::stream::StorageType::File,
        num_replicas: usize::try_from(replicas).unwrap_or(1),
        ..Default::default()
    })
    .await
    .map_err(|e| anyhow::anyhow!("ensure stream: {e}"))?;
    Ok(js)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    Published,
    /// 事件本身无效（毒消息）：进 DLQ 等人工修复。
    DeadLetter,
    /// 发布失败（NATS / 网络）：退避后重试，不限次数，不进 DLQ。
    Retry,
}

/// 发布结果的处理规则。重试次数不参与判断：网络故障持续多久都不该把好事件打进 DLQ。
fn disposition(published: bool, valid: bool) -> Disposition {
    if !valid {
        Disposition::DeadLetter
    } else if published {
        Disposition::Published
    } else {
        Disposition::Retry
    }
}

struct RelayOutcome {
    id: i64,
    lease_at: chrono::DateTime<chrono::Utc>,
    retry_count: i32,
    disposition: Disposition,
}

#[derive(Default)]
struct RelayBuckets {
    published_ids: Vec<i64>,
    published_leases: Vec<chrono::DateTime<chrono::Utc>>,
    dead_ids: Vec<i64>,
    dead_leases: Vec<chrono::DateTime<chrono::Utc>>,
    retry_ids: Vec<i64>,
    retry_leases: Vec<chrono::DateTime<chrono::Utc>>,
    repeated_retry_ids: Vec<i64>,
}

/// 按结果分桶并按 `id` 升序排列，保证单事务批量 `UPDATE ... FROM UNNEST` 锁序稳定、不与并发认领死锁。
fn partition_relay_outcomes(mut outcomes: Vec<RelayOutcome>) -> RelayBuckets {
    outcomes.sort_unstable_by_key(|o| o.id);
    let mut buckets = RelayBuckets::default();
    for item in outcomes {
        match item.disposition {
            Disposition::Published => {
                buckets.published_ids.push(item.id);
                buckets.published_leases.push(item.lease_at);
            }
            Disposition::DeadLetter => {
                buckets.dead_ids.push(item.id);
                buckets.dead_leases.push(item.lease_at);
            }
            Disposition::Retry => {
                if item.retry_count == 10 {
                    buckets.repeated_retry_ids.push(item.id);
                }
                buckets.retry_ids.push(item.id);
                buckets.retry_leases.push(item.lease_at);
            }
        }
    }
    buckets
}

/// relay 一批：outbox pending → JetStream 发布（确认后在单事务内批量标记 published / retry / DLQ）。
/// 返回本批行数。发布失败走 outbox 既有退避列。
pub async fn relay_once(pg: &PgPool, js: &jetstream::Context) -> anyhow::Result<usize> {
    // Claim with a 10-minute lease, then release PG locks before network I/O.
    let rows = sqlx::query!(r#"UPDATE billing_outbox o SET next_retry_at=now()+interval '10 minutes'
        FROM (SELECT id FROM billing_outbox WHERE status=0 AND ch_batch_id IS NULL
              AND (next_retry_at IS NULL OR next_retry_at<=now())
              AND NOT EXISTS(SELECT 1 FROM billing_ch_events e WHERE e.event_key='outbox:'||billing_outbox.event_id::text)
              ORDER BY id LIMIT 500 FOR UPDATE SKIP LOCKED) c
        WHERE o.id=c.id RETURNING o.id,o.event_id,o.topic,o.created_at,o.payload,o.retry_count,o.next_retry_at"#)
        .fetch_all(pg)
        .await?;
    let count = rows.len();
    if count == 0 {
        return Ok(0);
    }
    let outcomes = futures::stream::iter(rows)
        .map(|row| async move {
            let mut payload = row.payload;
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("_billing_event_id".into(), serde_json::json!(row.event_id));
                obj.insert(
                    "ts".into(),
                    serde_json::json!(row.created_at.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
                );
            }
            let event_key = delivery::outbox_key(row.event_id);
            let encoded = serde_json::to_vec(&payload).unwrap_or_default();
            let valid = row.topic.starts_with("billing.")
                && !row
                    .topic
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || matches!(b, b'*' | b'>'))
                && parse_event(&encoded, &event_key).is_ok();
            let traceparent = valid
                .then(|| traceparent_from_payload(&payload))
                .flatten();
            let ok = valid
                && tokio::time::timeout(Duration::from_secs(5), async {
                    let mut msg = jetstream::message::PublishMessage::build()
                        .message_id(event_key)
                        .payload(encoded.into());
                    if let Some(tp) = traceparent {
                        let mut headers = async_nats::HeaderMap::new();
                        headers.insert(
                            okapi_kernel::trace_context::TRACEPARENT_HEADER,
                            tp.as_str(),
                        );
                        msg = msg.headers(headers);
                    }
                    match js.send_publish(row.topic, msg).await {
                        Ok(ack) => ack.await.is_ok(),
                        Err(_) => false,
                    }
                })
                .await
                .unwrap_or(false);
            row.next_retry_at.map(|lease_at| RelayOutcome {
                id: row.id,
                lease_at,
                retry_count: row.retry_count,
                disposition: disposition(ok, valid),
            })
        })
        .buffer_unordered(32)
        .filter_map(std::future::ready)
        .collect::<Vec<_>>()
        .await;

    let buckets = partition_relay_outcomes(outcomes);
    apply_relay_buckets(pg, &buckets).await?;
    Ok(count)
}

async fn apply_relay_buckets(pg: &PgPool, buckets: &RelayBuckets) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    // 单事务按 lease CAS 批量落盘：把最多 500 次串行事务压成 1 次，且迟到的 ack 无法覆盖其他副本的新租约。
    if !buckets.published_ids.is_empty() {
        sqlx::query(
            "UPDATE billing_outbox o
             SET status = 1, published_at = now(), stats_protocol = 1, next_retry_at = NULL
             FROM UNNEST($1::bigint[], $2::timestamptz[]) AS c(id, lease_at)
             WHERE o.id = c.id
               AND o.status = 0
               AND o.ch_batch_id IS NULL
               AND o.next_retry_at = c.lease_at",
        )
        .bind(&buckets.published_ids)
        .bind(&buckets.published_leases)
        .execute(&mut *tx)
        .await?;
    }
    if !buckets.dead_ids.is_empty() {
        // 只有事件本身无效（毒消息）才进 DLQ。发布失败是 NATS / 网络的问题，不消耗毒消息次数。
        sqlx::query(
            "WITH claimed AS (
                 UPDATE billing_outbox o
                 SET status = 2, retry_count = o.retry_count + 1, next_retry_at = NULL
                 FROM UNNEST($1::bigint[], $2::timestamptz[]) AS c(id, lease_at)
                 WHERE o.id = c.id
                   AND o.status = 0
                   AND o.ch_batch_id IS NULL
                   AND o.next_retry_at = c.lease_at
                 RETURNING o.event_id, o.payload, o.retry_count
             )
             INSERT INTO billing_dlq (source, payload, error, retry_count, event_key)
             SELECT 'relay', payload, 'relay_invalid_event', retry_count, 'outbox:' || event_id::text
             FROM claimed
             ON CONFLICT (event_key) WHERE event_key IS NOT NULL DO NOTHING",
        )
        .bind(&buckets.dead_ids)
        .bind(&buckets.dead_leases)
        .execute(&mut *tx)
        .await?;
    }
    if !buckets.retry_ids.is_empty() {
        for outbox_id in &buckets.repeated_retry_ids {
            tracing::warn!(
                outbox_id,
                "outbox event still unpublished after repeated NATS failures; retrying with backoff"
            );
        }
        // 网络类失败：按退避一直重试（封顶 5 分钟），NATS 恢复后自动补发
        sqlx::query(
            "UPDATE billing_outbox o
             SET retry_count = o.retry_count + 1,
                 next_retry_at = now() + make_interval(secs => least(300, 5 * power(2, least(o.retry_count, 6))))
             FROM UNNEST($1::bigint[], $2::timestamptz[]) AS c(id, lease_at)
             WHERE o.id = c.id
               AND o.status = 0
               AND o.ch_batch_id IS NULL
               AND o.next_retry_at = c.lease_at",
        )
        .bind(&buckets.retry_ids)
        .bind(&buckets.retry_leases)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
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
    let cached = ensure_consumer(js).await?;
    let report = chsink_js_fetch_and_admit(pg, &cached, ch, Duration::from_secs(1)).await?;
    Ok(if report.fetched > 0 {
        report.fetched
    } else {
        delivered
    })
}

/// 复用已缓存的 JetStream 消费者拉取并投递一批；首轮顺带恢复滞留批次，追赶轮次跳过重复恢复并用短超时拉取。
pub(super) async fn chsink_js_with_consumer(
    pg: &PgPool,
    cached: &CachedConsumer,
    ch: &ChClient,
    fetch_expires: Duration,
    run_recovery: bool,
) -> anyhow::Result<JsBatchReport> {
    let mut delivered = 0;
    if run_recovery {
        super::chsink::recover_published(pg).await?;
        delivered = delivery::deliver_once(pg, ch).await?;
    }
    let mut report = chsink_js_fetch_and_admit(pg, cached, ch, fetch_expires).await?;
    report.delivered += delivered;
    Ok(report)
}

async fn chsink_js_fetch_and_admit(
    pg: &PgPool,
    cached: &CachedConsumer,
    ch: &ChClient,
    fetch_expires: Duration,
) -> anyhow::Result<JsBatchReport> {
    let mut batch = cached
        .consumer
        .fetch()
        .max_messages(BATCH_LIMIT)
        .expires(fetch_expires)
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
        return Ok(JsBatchReport::default());
    }
    let fetched = messages.len();
    let mut tx = pg.begin().await?;
    let mut events = Vec::with_capacity(fetched);
    for msg in &messages {
        // 取不到流序号（回复主题异常）的消息不能让整批报错：那样同一批会被反复投递、
        // 卡住后面所有消息直到 48h 过期。改用 payload 哈希作兜底键，照常进 DLQ / 入库。
        let legacy_key = match msg.info() {
            Ok(info) => format!("js:{}:{}", cached.stream_created, info.stream_sequence),
            Err(error) => {
                use sha2::Digest as _;
                tracing::warn!(%error, "JS message without stream identity; keyed by payload hash");
                format!(
                    "js:unknown:{}",
                    hex::encode(sha2::Sha256::digest(&msg.payload))
                )
            }
        };
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
            .into_iter()
            .map(|message| async move { message.double_ack().await }),
        Duration::from_secs(10),
    )
    .await;
    let delivered = delivery::deliver_once(pg, ch).await?;
    Ok(JsBatchReport { delivered, fetched })
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
    #[test]
    fn publish_failures_retry_forever_and_only_poison_dead_letters() {
        use super::{Disposition, disposition};
        assert_eq!(disposition(true, true), Disposition::Published);
        assert_eq!(disposition(false, true), Disposition::Retry);
        assert_eq!(disposition(false, false), Disposition::DeadLetter);
        assert_eq!(disposition(true, false), Disposition::DeadLetter);
    }

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

    #[test]
    fn relay_outcomes_partition_and_sort_by_id_for_stable_lock_order() {
        use super::{Disposition, RelayOutcome, partition_relay_outcomes};
        let t1 = chrono::DateTime::from_timestamp(1_700_000_001, 0).expect("ts");
        let t2 = chrono::DateTime::from_timestamp(1_700_000_002, 0).expect("ts");
        let buckets = partition_relay_outcomes(vec![
            RelayOutcome {
                id: 30,
                lease_at: t2,
                retry_count: 10,
                disposition: Disposition::Retry,
            },
            RelayOutcome {
                id: 10,
                lease_at: t1,
                retry_count: 0,
                disposition: Disposition::Published,
            },
            RelayOutcome {
                id: 25,
                lease_at: t2,
                retry_count: 0,
                disposition: Disposition::DeadLetter,
            },
            RelayOutcome {
                id: 5,
                lease_at: t2,
                retry_count: 2,
                disposition: Disposition::Published,
            },
            RelayOutcome {
                id: 12,
                lease_at: t1,
                retry_count: 3,
                disposition: Disposition::Retry,
            },
        ]);
        assert_eq!(buckets.published_ids, vec![5, 10]);
        assert_eq!(buckets.published_leases, vec![t2, t1]);
        assert_eq!(buckets.dead_ids, vec![25]);
        assert_eq!(buckets.retry_ids, vec![12, 30]);
        assert_eq!(buckets.repeated_retry_ids, vec![30]);
    }
}
