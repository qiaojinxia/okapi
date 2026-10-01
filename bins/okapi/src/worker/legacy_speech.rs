//! Bounded, resumable interpretation of retained old speech facts. Evidence is
//! persisted before progress; neither the financial ledger nor old MVs are edited.
use okapi_store::ChClient;
use serde_json::{Value, json};
use std::collections::HashMap;

const DIMS: [&str; 20] = [
    "ts",
    "request_id",
    "user_id",
    "api_key_id",
    "group_code",
    "model",
    "channel_id",
    "channel_key_id",
    "requested_model",
    "upstream_model",
    "endpoint",
    "upstream_endpoint",
    "node",
    "stream",
    "request_type",
    "billing_type",
    "log_type",
    "is_error",
    "client_type",
    "provider",
];

fn unsigned(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn columns() -> String {
    DIMS.into_iter()
        .chain([
            "prompt_tokens",
            "pricing_epoch",
            "ratio_snapshot",
            "media_units",
            "input_unit",
            "input_characters",
            "prompt_source",
            "completion_source",
            "upstream_prompt_tokens",
            "upstream_completion_tokens",
            "cache_read_reported",
            "cache_write_reported",
            "audio_prompt_reported",
            "image_prompt_reported",
            "audio_completion_reported",
            "image_completion_reported",
            "cache_read_audio_reported",
            "cache_read_image_reported",
            "cache_write_audio_reported",
            "cache_write_image_reported",
            "reasoning_reported",
        ])
        .chain(
            okapi_store::legacy_speech::ZERO_AXES
                .into_iter()
                .filter(|axis| {
                    !matches!(*axis, "cache_read_text_tokens" | "cache_write_text_tokens")
                }),
        )
        .collect::<Vec<_>>()
        .join(", ")
}

fn evidence(rows: &[Value]) -> anyhow::Result<Vec<Value>> {
    let mut evidence: HashMap<String, Value> = HashMap::new();
    for row in rows {
        let Some(characters) = okapi_store::legacy_speech::characters(row) else {
            continue;
        };
        let copies = unsigned(&row["copies"])
            .ok_or_else(|| anyhow::anyhow!("invalid legacy speech population"))?;
        let mut value = json!({});
        for dim in DIMS {
            value[dim] = row[dim].clone();
        }
        value["characters"] = json!(characters);
        value["snapshot_epoch"] = row["pricing_epoch"].clone();
        let identity = serde_json::to_string(&value)?;
        value["ratio_snapshot"] = row["ratio_snapshot"].clone();
        value["basis"] = json!(okapi_store::legacy_speech::BASIS);
        value["copies"] = json!(copies);
        if let Some(existing) = evidence.get_mut(&identity) {
            let count = unsigned(&existing["copies"])
                .and_then(|n| n.checked_add(copies))
                .ok_or_else(|| anyhow::anyhow!("legacy speech population overflow"))?;
            existing["copies"] = json!(count);
        } else {
            evidence.insert(identity, value);
        }
    }
    Ok(evidence.into_values().collect())
}

/// At most `limit` request identities. A completed v1 scan is not restarted:
/// late old outbox messages are normalized at ingestion with the same contract.
pub async fn process_once(ch: &ChClient, limit: u32) -> anyhow::Result<usize> {
    let progress=ch.query_json_each_row("SELECT toString(cursor_ts) AS cursor_ts,toString(cursor_id) AS cursor_id,complete,version FROM legacy_speech_calibration_v1 FINAL WHERE slot=1").await?;
    let previous = progress.first();
    if previous.is_some_and(|r| unsigned(&r["complete"]) == Some(1)) {
        return Ok(0);
    }
    let cursor_ts = previous
        .and_then(|r| r["cursor_ts"].as_str())
        .unwrap_or("1970-01-01 00:00:00.000");
    let cursor_id = previous
        .and_then(|r| r["cursor_id"].as_str())
        .unwrap_or("00000000-0000-0000-0000-000000000000");
    let version = previous
        .and_then(|r| unsigned(&r["version"]))
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("legacy speech cursor overflow"))?;
    let limit = limit.clamp(1, 500);
    let candidate = "endpoint='/v1/audio/speech' AND input_unit='' AND isNull(input_characters)";
    let sql = format!(
        "WITH page AS (SELECT ts,request_id FROM request_log_raw WHERE {candidate} AND (ts,request_id)>(toDateTime64({{cursor_ts:String}},3),toUUID({{cursor_id:String}})) GROUP BY ts,request_id ORDER BY ts,request_id LIMIT {limit}) SELECT {},count() AS copies FROM request_log_raw INNER JOIN page USING (ts,request_id) WHERE {candidate} GROUP BY ALL ORDER BY ts,request_id SETTINGS max_result_rows=10000,result_overflow_mode='throw'",
        columns()
    );
    let rows = ch
        .query_with_params(&sql, &[("cursor_ts", cursor_ts), ("cursor_id", cursor_id)])
        .await?;
    let values = evidence(&rows)?;
    if !values.is_empty() {
        ch.insert_json_each_row(
            "legacy_speech_units_v1",
            &values,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await?;
    }
    let last = rows.last();
    let progress = json!({"slot":1,"cursor_ts":last.and_then(|r|r["ts"].as_str()).unwrap_or(cursor_ts),
        "cursor_id":last.and_then(|r|r["request_id"].as_str()).unwrap_or(cursor_id),
        "complete":u8::from(rows.is_empty()),"version":version});
    ch.insert_json_each_row(
        "legacy_speech_calibration_v1",
        std::slice::from_ref(&progress),
        &uuid::Uuid::new_v4().to_string(),
    )
    .await?;
    Ok(rows.len())
}
