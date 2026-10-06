//! Direct outbox admission followed by delivery of persisted, immutable CH batches.
//! Transport retries and mode changes share the same event identity receipts.

use chrono::{DateTime, Utc};
use okapi_store::ChClient;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::delivery::{self, Event};

/// Retry existing batches first, then atomically claim a fresh batch.
pub async fn process_once(pg: &PgPool, ch: &ChClient) -> anyhow::Result<usize> {
    let delivered = delivery::deliver_once(pg, ch).await?;
    if delivered > 0 {
        return Ok(delivered);
    }
    let admitted = admit_outbox(pg, false).await?;
    if admitted == 0 {
        return Ok(0);
    }
    let delivered = delivery::deliver_once(pg, ch).await?;
    Ok(delivered.max(admitted))
}

/// Recover confirmed modern publications whose JS handoff has stalled.
pub(super) async fn recover_published(pg: &PgPool) -> anyhow::Result<usize> {
    admit_outbox(pg, true).await
}

async fn admit_outbox(pg: &PgPool, published_only: bool) -> anyhow::Result<usize> {
    let mut tx = pg.begin().await?;
    let rows = sqlx::query!(
        r#"SELECT id,event_id,created_at,payload FROM billing_outbox
           WHERE ch_batch_id IS NULL AND (
             ($2::boolean AND status=1 AND stats_protocol=1 AND published_at<=now()-interval '5 minutes')
             OR (NOT $2::boolean AND (
               (status=0 AND (next_retry_at IS NULL OR next_retry_at<=now()))
               OR (status=1 AND stats_protocol=1)
             ))
           )
           ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED"#,
        delivery::BATCH_LIMIT,
        published_only
    )
    .fetch_all(&mut *tx)
    .await?;
    if rows.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let events = rows
        .iter()
        .map(|r| Event {
            key: delivery::outbox_key(r.event_id),
            payload: r.payload.clone(),
            row: to_ch_row(r.created_at, &r.payload),
        })
        .collect();
    delivery::admit(&mut tx, events).await?;
    // An existing receipt may already be complete (or in DLQ); never reset it here.
    sqlx::query!(
        r#"UPDATE billing_outbox o SET ch_batch_id=b.id,status=b.status,
           retry_count=b.retry_count,next_retry_at=b.next_retry_at,
           published_at=CASE WHEN b.status=1 THEN COALESCE(o.published_at,now()) ELSE o.published_at END
           FROM billing_ch_events e JOIN billing_ch_batches b ON b.id=e.batch_id
           WHERE o.id=ANY($1) AND e.event_key='outbox:'||o.event_id::text"#,
        &ids
    )
    .execute(&mut *tx)
    .await?;
    // Crucial: this commit must precede CH I/O, so rollback cannot change the batch token.
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
    let historical_characters = okapi_store::legacy_speech::characters(payload);
    let (input_unit, input_characters) =
        historical_characters.map_or_else(|| input_units(payload), |n| ("characters", Some(n)));
    let log_type = get_i64(payload, "log_type");
    let is_stream = payload
        .get("is_stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut row = json!({
        "ts": ts,
        "ingested_at": Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
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
        "prompt_tokens": if historical_characters.is_some() {0} else {get_i64(payload, "prompt_tokens")},
        "cached_tokens": get_i64(payload, "cached_tokens"),
        // 旧 outbox 未记录缓存写入，保留 null，不能冒充已知的 0。
        "cache_write_tokens": payload.get("cache_write_tokens").cloned().unwrap_or(Value::Null),
        "cache_read_reported": payload.get("cache_read_reported").and_then(Value::as_bool).map(u8::from),
        "cache_write_reported": payload.get("cache_write_reported").and_then(Value::as_bool).map(u8::from),
        "completion_tokens": get_i64(payload, "completion_tokens"),
        "reasoning_tokens": get_i64(payload, "reasoning_tokens"),
        "media_units": "",
        "amount_micro": get_i64(payload, "amount_micro"),
        "original_amount_micro": get_i64(payload, "original_amount_micro"),
        "discount_micro": get_i64(payload, "discount_micro"),
        "upstream_cost_micro": get_i64(payload, "upstream_cost_micro"),
        "pricing_epoch": get_i64(payload, "pricing_epoch"),
        "ratio_snapshot": get_str(payload, "ratio_snapshot"),
        "latency_ms": duration_ms(payload, "latency_ms").unwrap_or_default(),
        "latency_reported": u8::from(duration_ms(payload, "latency_ms").is_some()),
        "ttft_ms": duration_ms(payload, "ttft_ms").unwrap_or_default(),
        "ttft_reported": u8::from(duration_ms(payload, "ttft_ms").is_some()),
        "stream": i32::from(is_stream),
        "retry_count": get_i64(payload, "retry_count"),
        "failover_count": get_i64(payload, "failover_count"),
        "sticky_layer": get_i64(payload, "sticky_layer"),
        "client_type": get_str(payload, "client_type"),
        "upstream_status": get_i64(payload, "upstream_status"),
        "error_code": get_str(payload, "error_code"),
        "is_error": i32::from(log_type == 5 || payload.pointer("/diagnostics/request_failed").and_then(Value::as_bool) == Some(true)),
    });
    row["diagnostics"] = json!(
        payload
            .get("diagnostics")
            .filter(|v| !v.is_null())
            .map(std::string::ToString::to_string)
            .unwrap_or_default()
    );
    row["billing_status"] = payload.get("status").cloned().unwrap_or(Value::Null);
    row["input_unit"] = json!(input_unit);
    row["input_characters"] = json!(input_characters);
    row["historical_prompt_units"] = json!(historical_characters);
    row["input_unit_basis"] = json!(if historical_characters.is_some() {
        okapi_store::legacy_speech::BASIS
    } else {
        ""
    });
    let extra = json!({
        "server_tool_usage": payload.get("server_tool_usage").filter(|v| !v.is_null()).map(Value::to_string).unwrap_or_default(),
        "cache_write_5m_tokens": payload.get("cache_write_5m_tokens"),
        "cache_write_1h_tokens": payload.get("cache_write_1h_tokens"),
        "audio_prompt_tokens": payload.get("audio_prompt_tokens"),
        "image_prompt_tokens": payload.get("image_prompt_tokens"),
        "audio_completion_tokens": payload.get("audio_completion_tokens"),
        "image_completion_tokens": payload.get("image_completion_tokens"),
        "cache_read_audio_tokens": payload.pointer("/cache_read_modalities/audio_tokens"),
        "cache_read_image_tokens": payload.pointer("/cache_read_modalities/image_tokens"),
        "cache_write_audio_tokens": payload.pointer("/cache_write_modalities/audio_tokens"),
        "cache_write_image_tokens": payload.pointer("/cache_write_modalities/image_tokens"),
        "prompt_source": usage_source(payload, "prompt_source"),
        "completion_source": usage_source(payload, "completion_source"),
        "upstream_prompt_tokens": payload.pointer("/upstream_usage/prompt_tokens"),
        "upstream_completion_tokens": payload.pointer("/upstream_usage/completion_tokens"),
        "requested_model": get_str(payload, "requested_model"),
        "upstream_model": get_str(payload, "upstream_model"),
        "endpoint": get_str(payload, "endpoint"),
        "upstream_endpoint": get_str(payload, "upstream_endpoint"),
        "billing_type": get_str(payload, "billing_type"),
        "request_type": match (get_str(payload, "endpoint"), payload.get("is_stream").and_then(Value::as_bool)) {
            ("/v1/realtime", _) => "websocket", (_, Some(true)) => "stream", (_, Some(false)) => "non_stream", _ => "",
        },
        "upstream_cost_known": u8::from(payload.get("upstream_cost_known").and_then(Value::as_bool) == Some(true)),
        // 结算来源池（§11.28）：0 钱包 1 订阅；老载荷缺省 0
        "pool": u8::from(payload.get("pool").and_then(Value::as_i64) == Some(1)),
    });
    if let (Some(row), Some(extra)) = (row.as_object_mut(), extra.as_object()) {
        row.extend(extra.clone());
    }
    add_detail_observations(&mut row, payload);
    row
}

// Explicit metadata must be coherent; historical interpretation is a separate strict contract.
fn input_units(payload: &Value) -> (&'static str, Option<u32>) {
    let characters = payload.get("input_characters");
    match get_str(payload, "input_unit") {
        "tokens" if characters.is_none_or(Value::is_null) => ("tokens", None),
        "characters" => {
            let count = characters
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
            let no_tokens = [
                "prompt_tokens",
                "completion_tokens",
                "cached_tokens",
                "cache_write_tokens",
                "reasoning_tokens",
                "audio_prompt_tokens",
                "audio_completion_tokens",
                "image_prompt_tokens",
                "image_completion_tokens",
            ]
            .iter()
            .all(|field| {
                payload
                    .get(field)
                    .is_none_or(|v| v.is_null() || v.as_u64() == Some(0))
            });
            if no_tokens && let Some(count) = count {
                return ("characters", Some(count));
            }
            ("", None)
        }
        _ => ("", None),
    }
}

fn add_detail_observations(row: &mut Value, payload: &Value) {
    for (field, pointer) in [
        ("audio_prompt_reported", "/reported_details/prompt/audio"),
        ("image_prompt_reported", "/reported_details/prompt/image"),
        (
            "audio_completion_reported",
            "/reported_details/completion/audio",
        ),
        (
            "image_completion_reported",
            "/reported_details/completion/image",
        ),
        (
            "cache_read_audio_reported",
            "/reported_details/cache_read/audio",
        ),
        (
            "cache_read_image_reported",
            "/reported_details/cache_read/image",
        ),
        (
            "cache_write_audio_reported",
            "/reported_details/cache_write/audio",
        ),
        (
            "cache_write_image_reported",
            "/reported_details/cache_write/image",
        ),
        ("reasoning_reported", "/reported_details/reasoning"),
    ] {
        row[field] = payload
            .pointer(pointer)
            .and_then(Value::as_bool)
            .map_or(Value::Null, |value| json!(u8::from(value)));
    }
}

// Invalid metadata is unknown, never a measured zero or an overflowing CH UInt32.
fn duration_ms(payload: &Value, field: &str) -> Option<u32> {
    payload
        .get(field)?
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
}

fn usage_source<'a>(payload: &'a Value, field: &str) -> &'a str {
    match get_str(payload, field) {
        value @ ("upstream" | "estimated" | "local_override") => value,
        _ => "unknown",
    }
}

#[cfg(test)]
mod unit_tests {
    use super::js_payload_to_ch_row;
    use serde_json::json;

    #[test]
    fn diagnostics_preserve_charged_stream_failures_and_timing_observation() {
        let diagnostics = json!({"request_failed":true,"error_message":"broken stream","attempts":[{"status":200}]});
        let row = js_payload_to_ch_row(&json!({
            "status":20,"log_type":2,"amount_micro":1234,"latency_ms":0,"ttft_ms":null,"diagnostics":diagnostics,
        }));
        assert_eq!(row["is_error"], 1);
        assert_eq!(row["billing_status"], 20);
        assert_eq!(row["amount_micro"], 1234);
        assert_eq!(row["latency_reported"], 1);
        assert_eq!(row["ttft_reported"], 0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(row["diagnostics"].as_str().unwrap())
                .unwrap(),
            diagnostics
        );
        let legacy = js_payload_to_ch_row(&json!({"log_type":2,"amount_micro":1234}));
        assert_eq!(legacy["is_error"], 0);
        assert_eq!(legacy["diagnostics"], "");
        assert!(legacy["billing_status"].is_null());
    }

    #[test]
    fn input_units_preserve_explicit_zero_and_do_not_guess_old_characters() {
        for count in [0, 11, u32::MAX] {
            let row = js_payload_to_ch_row(
                &json!({"input_unit":"characters","input_characters":count,"prompt_tokens":0,"completion_tokens":0}),
            );
            assert_eq!(row["input_unit"], "characters");
            assert_eq!(row["input_characters"], count);
        }
        let row = js_payload_to_ch_row(&json!({"endpoint":"/v1/audio/speech","prompt_tokens":11}));
        assert_eq!(row["input_unit"], "");
        assert!(row["input_characters"].is_null());
        assert_eq!(row["prompt_tokens"], 11, "raw history remains auditable");
    }

    #[test]
    fn invalid_or_conflicting_input_units_stay_unknown() {
        for payload in [
            json!({"input_unit":"characters"}),
            json!({"input_unit":"characters","input_characters":11,"prompt_tokens":11}),
            json!({"input_unit":"characters","input_characters":11,"audio_completion_tokens":1}),
            json!({"input_unit":"characters","input_characters":"11"}),
            json!({"input_unit":"characters","input_characters":u64::from(u32::MAX)+1}),
            json!({"input_unit":"tokens","input_characters":11}),
            json!({"input_unit":"other","input_characters":11}),
        ] {
            let row = js_payload_to_ch_row(&payload);
            assert_eq!(row["input_unit"], "", "{payload}");
            assert!(row["input_characters"].is_null());
        }
    }
}
