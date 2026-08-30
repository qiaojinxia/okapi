//! chsink：billing_outbox → ClickHouse request_log_raw 批写。
//!
//! 单机直连形态（docs/database.md §4.2：无 NATS 时 worker 直接消费 outbox）；
//! NATS JetStream 传输在 M2 后续批次接入后，本模块变为 JetStream 消费者。
//!
//! 幂等：批次 dedup_token = "outbox-<min_id>-<max_id>"。已知边界：CH 写成功但
//! PG 标记失败的窗口内，若重试批次成员发生变化（新行混入），token 变化会导致
//! 少量明细重复——账本不受影响，统计侧由 CH↔PG 对账检出；NATS 序号接入后消除。

use chrono::{DateTime, Utc};
use okapi_store::ChClient;
use serde_json::{Value, json};
use sqlx::PgPool;

const BATCH_LIMIT: i64 = 500;
const MAX_RETRY: i32 = 5;

/// 处理一批待投递 outbox。返回本批行数（0 = 无待处理）。
pub async fn process_once(pg: &PgPool, ch: &ChClient) -> anyhow::Result<usize> {
    let mut tx = pg.begin().await?;
    let rows = sqlx::query!(
        r#"
        SELECT id, created_at, payload
        FROM billing_outbox
        WHERE status = 0 AND (next_retry_at IS NULL OR next_retry_at <= now())
        ORDER BY id
        LIMIT $1
        FOR UPDATE SKIP LOCKED
        "#,
        BATCH_LIMIT
    )
    .fetch_all(&mut *tx)
    .await?;

    if rows.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let first = ids.first().copied().unwrap_or(0);
    let last = ids.last().copied().unwrap_or(0);
    let dedup_token = format!("outbox-{first}-{last}");
    let ch_rows: Vec<Value> = rows
        .iter()
        .map(|r| to_ch_row(r.created_at, &r.payload))
        .collect();

    match ch
        .insert_json_each_row("request_log_raw", &ch_rows, &dedup_token)
        .await
    {
        Ok(()) => {
            sqlx::query!(
                r#"UPDATE billing_outbox SET status = 1, published_at = now() WHERE id = ANY($1)"#,
                &ids
            )
            .execute(&mut *tx)
            .await?;
        }
        Err(err) => {
            let err_text = err.to_string();
            tracing::warn!(error = %err_text, batch = ids.len(), "chsink 批写失败，退避重试");
            // 指数退避（5s 起，封顶 300s）；超过 MAX_RETRY 次转 DLQ 终态
            sqlx::query!(
                r#"
                UPDATE billing_outbox
                SET retry_count = retry_count + 1,
                    next_retry_at = now() + make_interval(secs => least(300, 5 * power(2, retry_count))),
                    status = CASE WHEN retry_count + 1 >= $2 THEN 2 ELSE 0 END
                WHERE id = ANY($1)
                "#,
                &ids,
                MAX_RETRY
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                r#"
                INSERT INTO billing_dlq (source, payload, error, retry_count)
                SELECT 'chsink', payload, $2, retry_count
                FROM billing_outbox
                WHERE id = ANY($1) AND status = 2
                "#,
                &ids,
                err_text
            )
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;
    Ok(ids.len())
}

fn get_i64(payload: &Value, key: &str) -> i64 {
    payload.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn get_str<'a>(payload: &'a Value, key: &str) -> &'a str {
    payload.get(key).and_then(Value::as_str).unwrap_or("")
}

/// outbox payload → request_log_raw 行（列集见 docs/database.md §3.1）。
fn to_ch_row(created_at: DateTime<Utc>, payload: &Value) -> Value {
    build_ch_row(
        &created_at.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        payload,
    )
}

/// JetStream 消息 payload → CH 行（relay 发布时已内嵌 "ts"）。
pub fn js_payload_to_ch_row(payload: &Value) -> Value {
    let ts = payload
        .get("ts")
        .and_then(Value::as_str)
        .unwrap_or("1970-01-01 00:00:00.000")
        .to_owned();
    build_ch_row(&ts, payload)
}

fn build_ch_row(ts: &str, payload: &Value) -> Value {
    let log_type = get_i64(payload, "log_type");
    let is_stream = payload
        .get("is_stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!({
        "ts": ts,
        "request_id": get_str(payload, "request_id"),
        "upstream_request_id": get_str(payload, "upstream_request_id"),
        "log_type": log_type,
        "user_id": get_i64(payload, "user_id"),
        "api_key_id": get_i64(payload, "api_key_id"),
        "team_id": 0,
        "group_code": get_str(payload, "group"),
        "model": get_str(payload, "model"),
        "channel_id": get_i64(payload, "channel_id"),
        "channel_key_id": get_i64(payload, "channel_key_id"),
        "provider": "",
        "client_ip": get_str(payload, "client_ip"),
        "node": get_str(payload, "node"),
        "prompt_tokens": get_i64(payload, "prompt_tokens"),
        "cached_tokens": get_i64(payload, "cached_tokens"),
        "completion_tokens": get_i64(payload, "completion_tokens"),
        "reasoning_tokens": get_i64(payload, "reasoning_tokens"),
        "media_units": "",
        "amount_micro": get_i64(payload, "amount_micro"),
        "original_amount_micro": get_i64(payload, "original_amount_micro"),
        "discount_micro": get_i64(payload, "discount_micro"),
        "upstream_cost_micro": 0,
        "pricing_epoch": get_i64(payload, "pricing_epoch"),
        "ratio_snapshot": get_str(payload, "ratio_snapshot"),
        "latency_ms": get_i64(payload, "latency_ms"),
        "ttft_ms": get_i64(payload, "ttft_ms"),
        "stream": i32::from(is_stream),
        "retry_count": get_i64(payload, "retry_count"),
        "failover_count": get_i64(payload, "failover_count"),
        "sticky_layer": get_i64(payload, "sticky_layer"),
        "client_type": get_str(payload, "client_type"),
        "upstream_status": get_i64(payload, "upstream_status"),
        "error_code": get_str(payload, "error_code"),
        "is_error": i32::from(log_type == 5),
    })
}
