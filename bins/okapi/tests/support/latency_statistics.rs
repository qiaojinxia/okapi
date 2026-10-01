use super::ttft_statistics::{insert, row};
use super::{Env, get, setup_with_ch_database};
use futures::FutureExt as _;
use okapi::worker::chsink;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

fn event(env: &Env, ms: Value, output: i64, error: bool) -> Value {
    let mut payload = super::payload(env, 1000, 250, 100, error);
    payload["ts"] = row(env, json!(100), true)["ts"].clone();
    payload["latency_ms"] = ms;
    payload["completion_tokens"] = json!(output);
    payload["is_stream"] = json!(!error);
    payload
}

async fn request(env: &Env, path: &str, portal: bool) -> Value {
    let (status, body) = get(
        env,
        path,
        if portal {
            &env.user_token
        } else {
            &env.super_token
        },
    )
    .await;
    assert_eq!(status, 200, "{path}: {body}");
    body
}

fn check(value: &Value, reqs: i64, samples: i64, sum: i64, output: i64, observed: i64) {
    for (key, expected) in [
        ("requests", reqs),
        ("latency_samples", samples),
        ("latency_sum_ms", sum),
        ("performance_completion_tokens", output),
        ("latency_observed_requests", observed),
    ] {
        assert_eq!(value[key], expected, "{key}: {value}");
    }
    let complete = observed == reqs;
    assert_eq!(value["latency_history_complete"], complete);
    assert_eq!(
        value["avg_latency_ms"],
        if complete && samples > 0 {
            json!(sum / samples)
        } else {
            Value::Null
        },
        "{value}"
    );
    assert_eq!(
        value["tokens_per_1k_sec"],
        if complete && sum > 0 {
            json!(output * 1_000_000 / sum)
        } else {
            Value::Null
        },
        "{value}"
    );
}

async fn totals(env: &Env) -> Vec<Value> {
    let admin = request(
        env,
        &format!("/admin/stats/trend?days=2&user_id={}", env.user_id),
        false,
    )
    .await;
    let portal = request(env, "/api/me/stats/breakdown?days=2&scope=user", true).await;
    vec![admin["total"].clone(), portal["total"].clone()]
}

async fn quality(env: &Env) -> Vec<Value> {
    let mut values = Vec::new();
    for path in [
        "/admin/stats/models?days=2&limit=100".to_owned(),
        "/admin/stats/channels?days=2&limit=100".to_owned(),
        format!("/admin/stats/channels/{}/timeline?hours=48", env.channel_id),
    ] {
        let body = request(env, &path, false).await;
        let own = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r["model"] == env.model
                    || r["channel_id"] == env.channel_id
                    || r.get("bucket").is_some()
            })
            .unwrap_or_else(|| panic!("missing fixture: {body}"));
        values.push(own.clone());
    }
    values
}

#[test]
fn duration_metadata_distinguishes_unknown_zero_and_numeric_bounds() {
    for input in [
        Value::Null,
        json!(-1),
        json!(1.5),
        json!("100"),
        json!(u64::MAX),
    ] {
        let value = chsink::js_payload_to_ch_row(&json!({"latency_ms":input,"ttft_ms":input}));
        for prefix in ["latency", "ttft"] {
            assert_eq!(value[format!("{prefix}_ms")], 0);
            assert_eq!(value[format!("{prefix}_reported")], 0);
        }
    }
    for input in [0, u32::MAX] {
        let value = chsink::js_payload_to_ch_row(&json!({"latency_ms":input,"ttft_ms":input}));
        assert_eq!(value["latency_ms"], input);
        assert_eq!(value["latency_reported"], 1);
        assert_eq!(value["ttft_ms"], input);
        assert_eq!(value["ttft_reported"], 1);
    }
}

#[tokio::test]
async fn measured_duration_and_paired_output_agree_across_statistics_and_retention() {
    let database = format!("okapi_latency_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env
        .state
        .ch
        .as_ref()
        .expect("latency integration requires ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_samples(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_samples(env: &Env, ch: &ChClient) {
    let (channel, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("latency-{}", Uuid::new_v4()),
        "openai",
        "http://127.0.0.1:1",
        "fixture",
        &[&env.model],
        false,
        None,
    )
    .await
    .unwrap();
    let mut events = vec![
        event(env, json!(0), 20, false),
        event(env, json!(1001), 200, true),
        event(env, json!(2999), 380, false),
    ];
    events[0]["node"] = json!("zero");
    for input in [
        Value::Null,
        json!(-1),
        json!(1.5),
        json!("100"),
        json!(u64::MAX),
    ] {
        let mut missing = event(env, input, 10_000, false);
        missing["node"] = json!("missing");
        events.push(missing);
    }
    seed_pg(env, &events).await;
    let mut rows: Vec<_> = events.iter().map(chsink::js_payload_to_ch_row).collect();
    rows[2]["channel_id"] = json!(channel);
    rows[2]["model"] = json!(format!("{}-other", env.model));
    for (suffix, ms) in [("zero", json!(0)), ("unknown", Value::Null)] {
        let mut other = chsink::js_payload_to_ch_row(&event(env, ms, 8000, false));
        other["model"] = json!(format!("{}-{suffix}", env.model));
        other["channel_id"] = json!(0);
        other["user_id"] = json!(env.user_id + 1_000_000);
        rows.push(other);
    }
    insert(ch, &rows).await;
    ch.ensure_schema().await.unwrap();
    check_endpoints(env).await;
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_endpoints(env).await;
}

async fn seed_pg(env: &Env, events: &[Value]) {
    let me = request(env, "/api/me", true).await;
    let key = me["key_id"].as_i64().unwrap();
    for item in events {
        let duration = item["latency_ms"]
            .as_u64()
            .and_then(|n| i32::try_from(n).ok());
        sqlx::query("INSERT INTO billing_records(request_id,user_id,api_key_id,model_name,status,latency_ms,completion_tokens) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(env.user_id).bind(key).bind(&env.model).bind(if item["log_type"]==5 {40_i16} else {20}).bind(duration).bind(i32::try_from(item["completion_tokens"].as_i64().unwrap()).unwrap())
            .execute(&env.pg).await.unwrap();
    }
    let result = request(env, "/api/me/logs/stat?scope=user", true).await;
    assert_eq!(result["avg_latency_ms"], 1333);
    assert_eq!(result["latency_samples"], 3);
}

async fn check_endpoints(env: &Env) {
    for value in totals(env).await {
        check(&value, 8, 3, 4000, 600, 8);
        assert_eq!(value["completion_tokens"], 50_600);
        assert_eq!(value["amount_micro"], 8000);
    }
    for value in quality(env).await {
        check(&value, 7, 2, 1001, 220, 7);
        assert_eq!(value["latency_p50_ms"], 500);
    }
    let query = format!("days=2&user_id={}", env.user_id);
    let provider = request(
        env,
        &format!("/admin/stats/breakdown?{query}&by=provider"),
        false,
    )
    .await;
    check(&provider["data"][0], 8, 3, 4000, 600, 8);
    let stacked = request(
        env,
        &format!("/admin/stats/trend?{query}&stack=model&limit=1"),
        false,
    )
    .await;
    check(
        &stacked["data"][0]["values"][&env.model],
        7,
        2,
        1001,
        220,
        7,
    );
    check(&stacked["data"][0]["values"]["__other"], 1, 1, 2999, 380, 1);
    for (node, reqs, samples, output) in [("zero", 1, 1, 20), ("missing", 5, 0, 0)] {
        let result = request(
            env,
            &format!("/admin/stats/trend?{query}&node={node}"),
            false,
        )
        .await;
        check(&result["total"], reqs, samples, 0, output, reqs);
    }
    let models = request(env, "/admin/stats/models?days=2&limit=100", false).await;
    for (suffix, samples, output) in [("zero", 1, 8000), ("unknown", 0, 0)] {
        let model = format!("{}-{suffix}", env.model);
        let value = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["model"] == model)
            .unwrap();
        check(value, 1, samples, 0, output, 1);
        assert_eq!(
            value["latency_p50_ms"],
            if samples > 0 { json!(0) } else { Value::Null }
        );
    }
}

#[tokio::test]
async fn latency_upgrade_recovers_complete_raw_and_marks_unrecoverable_history() {
    for legacy_dimensions in [false, true] {
        let database = format!("okapi_latency_upgrade_{}", Uuid::new_v4().simple());
        let env = setup_with_ch_database(&database).await;
        let ch = env
            .state
            .ch
            .as_ref()
            .expect("latency integration requires ClickHouse");
        ch.ensure_schema().await.unwrap();
        let result = std::panic::AssertUnwindSafe(check_upgrade(&env, ch, legacy_dimensions))
            .catch_unwind()
            .await;
        ch.execute(&format!("DROP DATABASE {database} SYNC"))
            .await
            .unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}

async fn check_upgrade(env: &Env, ch: &ChClient, legacy_dimensions: bool) {
    for view in [
        "mv_latency_reporting_hour",
        "mv_model_latency_hour",
        "mv_channel_latency_5min",
        "mv_output_rate_5min",
    ] {
        ch.execute(&format!("DROP TABLE {view} SYNC"))
            .await
            .unwrap();
    }
    if legacy_dimensions {
        ch.execute("DROP TABLE mv_analysis_hour SYNC")
            .await
            .unwrap();
    }
    ch.execute("ALTER TABLE request_log_raw DROP COLUMN latency_reported")
        .await
        .unwrap();
    let mut legacy: Vec<_> = [(1000, 100), (3000, 300), (0, 9000)]
        .into_iter()
        .map(|(ms, output)| chsink::js_payload_to_ch_row(&event(env, json!(ms), output, false)))
        .collect();
    for value in &mut legacy {
        value.as_object_mut().unwrap().remove("latency_reported");
    }
    insert(ch, &legacy).await;
    ch.ensure_schema().await.unwrap();
    insert(
        ch,
        &[chsink::js_payload_to_ch_row(&event(
            env,
            json!(0),
            20,
            false,
        ))],
    )
    .await;
    let mut values = totals(env).await;
    values.extend(quality(env).await);
    for value in values {
        check(&value, 4, 3, 4000, 420, 4);
    }
    ch.execute("ALTER TABLE request_log_raw DELETE WHERE latency_reported IS NULL SETTINGS mutations_sync=1").await.unwrap();
    let mut values = totals(env).await;
    values.extend(quality(env).await);
    for value in values {
        check(&value, 4, 1, 0, 20, 1);
        assert_eq!(value["latency_history_coverage_bp"], 2500);
        if value.get("latency_p50_ms").is_some() {
            assert!(value["latency_p50_ms"].is_null());
        }
    }
}
