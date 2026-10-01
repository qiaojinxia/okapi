use super::{Env, get, payload, setup_with_ch_database};
use futures::FutureExt as _;
use okapi::worker::chsink;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

pub(super) fn row(env: &Env, ttft: Value, stream: bool) -> Value {
    let mut payload = payload(env, 1_000, 250, 0, false);
    payload["ttft_ms"] = ttft;
    payload["is_stream"] = json!(stream);
    // Fixed bucket away from a boundary, shared by the entire fixture.
    payload["ts"] = json!(
        (chrono::Utc::now() - chrono::Duration::hours(2))
            .format("%Y-%m-%d %H:00:00.000")
            .to_string()
    );
    chsink::js_payload_to_ch_row(&payload)
}

pub(super) async fn insert(ch: &ChClient, rows: &[Value]) {
    let token = Uuid::new_v4().to_string();
    ch.insert_json_each_row("request_log_raw", rows, &token)
        .await
        .unwrap();
    // Same batch replay must not duplicate samples in either old or new MVs.
    ch.insert_json_each_row("request_log_raw", rows, &token)
        .await
        .unwrap();
}

async fn quality_rows(env: &Env) -> Vec<Value> {
    let mut result = Vec::new();
    for path in [
        "/admin/stats/models?days=1&limit=100".to_owned(),
        "/admin/stats/channels?days=1&limit=100".to_owned(),
        format!("/admin/stats/channels/{}/timeline?hours=24", env.channel_id),
    ] {
        let (status, body) = get(env, &path, &env.super_token).await;
        assert_eq!(status, 200, "{path}: {body}");
        let row = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| {
                row["model"] == env.model
                    || row["channel_id"] == env.channel_id
                    || row.get("bucket").is_some()
            })
            .unwrap_or_else(|| panic!("missing own row in {path}: {body}"));
        result.push(row.clone());
    }
    result
}

fn assert_quality(rows: &[Value], requests: i64, samples: i64, value: &Value, source: &str) {
    for row in rows {
        assert_eq!(row["requests"], requests, "{row}");
        assert_eq!(row["ttft_samples"], samples, "{row}");
        for field in ["ttft_p50_ms", "ttft_p95_ms", "ttft_p99_ms"] {
            assert_eq!(&row[field], value, "{field}: {row}");
        }
        assert_eq!(row["ttft_source"], source, "{row}");
        assert_eq!(row["ttft_history_complete"], source != "incomplete");
    }
    // Token and money totals stay on their original aggregates throughout upgrade.
    assert_eq!(rows[0]["tokens"], requests * 300);
    assert_eq!(rows[0]["amount_micro"], requests * 1_000);
    assert_eq!(rows[1]["amount_micro"], requests * 1_000);
}

#[tokio::test]
async fn ttft_filters_missing_and_non_stream_samples_preserving_measured_zero() {
    let database = format!("okapi_ttft_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let Some(ch) = env.state.ch.as_ref() else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    };
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_samples(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_samples(env: &Env, ch: &ChClient) {
    let mut rows: Vec<Value> = (0..30).map(|_| row(env, Value::Null, true)).collect();
    rows.push(row(env, json!(1_000), true));
    rows.push(row(env, json!(9_999), false));
    for (model, ttft) in [("measured-zero'\\", json!(0)), ("unknown", Value::Null)] {
        let mut extra = row(env, ttft, true);
        extra["model"] = json!(model);
        extra["channel_id"] = json!(0);
        rows.push(extra);
    }
    let mut outside = row(env, json!(80_000), true);
    outside["ts"] = json!(
        (chrono::Utc::now() - chrono::Duration::days(3))
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string()
    );
    rows.push(outside);
    insert(ch, &rows).await;
    ch.ensure_schema().await.unwrap();
    assert_quality(&quality_rows(env).await, 32, 1, &json!(1_000), "aggregate");
    let (status, models) = get(env, "/admin/stats/models?days=1", &env.super_token).await;
    assert_eq!(status, 200, "{models}");
    for (model, value, samples) in [
        ("measured-zero'\\", json!(0), 1),
        ("unknown", Value::Null, 0),
    ] {
        let row = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["model"] == model)
            .unwrap();
        assert_eq!(row["ttft_p50_ms"], value, "{row}");
        assert_eq!(row["ttft_samples"], samples);
        assert_eq!(row["ttft_history_coverage_bp"], 10_000);
    }
    // Long-lived aggregates must work after raw retention removes every detail.
    super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
        .await
        .unwrap();
    assert_quality(&quality_rows(env).await, 32, 1, &json!(1_000), "aggregate");
}

#[tokio::test]
async fn ttft_upgrade_recovers_legacy_or_marks_incomplete_without_losing_totals() {
    let database = format!("okapi_ttft_upgrade_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let Some(ch) = env.state.ch.as_ref() else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    };
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_upgrade(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_upgrade(env: &Env, ch: &ChClient) {
    // Simulate pre-upgrade storage without the new views or receipt column.
    for view in [
        "mv_model_ttft_hour",
        "mv_channel_ttft_5min",
        "mv_ttft_reporting_hour",
    ] {
        super::population_storage::execute(ch, &format!("DROP TABLE {view} SYNC"))
            .await
            .unwrap();
    }
    super::population_storage::execute(ch, "ALTER TABLE request_log_raw DROP COLUMN ttft_reported")
        .await
        .unwrap();
    let mut legacy: Vec<Value> = (0..12).map(|_| row(env, Value::Null, true)).collect();
    legacy.push(row(env, json!(1_000), true));
    legacy.push(row(env, json!(8_000), false));
    for row in &mut legacy {
        row.as_object_mut().unwrap().remove("ttft_reported");
    }
    insert(ch, &legacy).await;
    ch.ensure_schema().await.unwrap();
    insert(ch, &[row(env, json!(1_000), true)]).await;
    ch.ensure_schema().await.unwrap();
    let recovered = quality_rows(env).await;
    assert_quality(&recovered, 15, 2, &json!(1_000), "raw");
    for row in recovered {
        assert_eq!(row["ttft_observed_requests"], 15);
        assert_eq!(row["ttft_history_coverage_bp"], 10_000);
    }
    // Deletion is confined to this test's unique DB. Old monetary/token MVs stay.
    super::population_storage::execute(
        ch,
        "ALTER TABLE request_log_raw DELETE WHERE ttft_reported IS NULL \
         SETTINGS mutations_sync = 1",
    )
    .await
    .unwrap();
    let incomplete = quality_rows(env).await;
    assert_quality(&incomplete, 15, 1, &Value::Null, "incomplete");
    for row in incomplete {
        assert_eq!(row["ttft_observed_requests"], 1);
        assert_eq!(row["ttft_history_coverage_bp"], 666);
    }
}
