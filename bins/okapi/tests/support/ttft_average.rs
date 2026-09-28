use super::ttft_statistics::{insert, row};
use super::{Env, get, setup, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_average(value: &Value, requests: i64, samples: i64, sum: i64, observed: i64) {
    assert_eq!(value["requests"], requests, "{value}");
    assert_eq!(value["ttft_samples"], samples, "{value}");
    assert_eq!(value["ttft_sum_ms"], sum, "{value}");
    assert_eq!(value["ttft_observed_requests"], observed, "{value}");
    assert_eq!(
        value["ttft_history_complete"],
        requests == observed,
        "{value}"
    );
    assert_eq!(
        value["avg_ttft_ms"],
        if samples > 0 && observed == requests {
            json!(sum / samples)
        } else {
            Value::Null
        },
        "{value}"
    );
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

async fn totals(env: &Env) -> Vec<Value> {
    let admin = request(
        env,
        &format!("/admin/stats/trend?days=2&user_id={}", env.user_id),
        false,
    )
    .await;
    let portal = request(env, "/api/me/stats/breakdown?scope=user&days=2", true).await;
    vec![admin["total"].clone(), portal["total"].clone()]
}

#[tokio::test]
async fn average_ttft_filters_samples_and_recombines_weighted_groups_after_retention() {
    let database = format!("okapi_ttft_mean_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env
        .state
        .ch
        .as_ref()
        .expect("TTFT integration requires ClickHouse");
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
        &format!("mean-{}", Uuid::new_v4()),
        "openai",
        "http://127.0.0.1:1",
        "fixture",
        &[&env.model],
        false,
        None,
    )
    .await
    .unwrap();
    let mut rows = vec![
        row(env, json!(0), true),
        row(env, json!(1001), true),
        row(env, Value::Null, true),
        row(env, json!(9999), false),
        row(env, json!(777), true),
    ];
    rows[0]["node"] = json!("zero");
    rows[4]["ttft_reported"] = json!(0);
    for value in [json!(2999), Value::Null] {
        let mut extra = row(env, value, true);
        extra["model"] = json!(format!("{}-other", env.model));
        extra["channel_id"] = json!(channel);
        rows.push(extra);
    }
    let mut other_owner = row(env, json!(80000), true);
    other_owner["user_id"] = json!(env.user_id + 1_000_000);
    rows.push(other_owner);
    for record in &mut rows {
        record["cache_read_reported"] = json!(1);
    }
    insert(ch, &rows).await;
    ch.ensure_schema().await.unwrap();
    check_sample_endpoints(env).await;
    // Correct sums and zero-valued samples survive raw retention and repeated schema setup.
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_sample_endpoints(env).await;
    // A source ahead of the authoritative request total must not be presented as complete.
    ch.execute("INSERT INTO mv_ttft_reporting_hour SELECT * FROM mv_ttft_reporting_hour")
        .await
        .unwrap();
    for total in totals(env).await {
        assert_average(&total, 7, 0, 0, 0);
    }
}

async fn check_sample_endpoints(env: &Env) {
    for total in totals(env).await {
        assert_average(&total, 7, 3, 4000, 7);
        assert_eq!(total["tokens"], 2100);
        assert_eq!(total["amount_micro"], 7000);
    }
    let query = format!("days=2&user_id={}", env.user_id);
    let provider = request(
        env,
        &format!("/admin/stats/breakdown?{query}&by=provider"),
        false,
    )
    .await;
    assert_eq!(provider["data"].as_array().unwrap().len(), 1);
    assert_average(&provider["data"][0], 7, 3, 4000, 7);
    let stacked = request(
        env,
        &format!("/admin/stats/trend?{query}&stack=model&limit=1"),
        false,
    )
    .await;
    let values = &stacked["data"][0]["values"];
    assert_average(&values[&env.model], 5, 2, 1001, 5);
    assert_average(&values["__other"], 2, 1, 2999, 2);
    assert_eq!(values[&env.model]["cache_hit_bp"], 0);
    assert_eq!(values["__other"]["cache_hit_bp"], 0);
    for (filter, requests, samples, sum) in [("stream=false", 1, 0, 0), ("node=zero", 1, 1, 0)] {
        let filtered = request(env, &format!("/admin/stats/trend?{query}&{filter}"), false).await;
        assert_average(&filtered["total"], requests, samples, sum, requests);
    }
    let activity = request(env, "/api/me/stats/activity?scope=user", true).await;
    let own = activity["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["model"] == env.model)
        .unwrap();
    assert_average(own, 5, 2, 1001, 5);
}

#[tokio::test]
async fn average_ttft_upgrade_recovers_raw_and_exposes_incomplete_history() {
    for legacy_dimensions in [false, true] {
        let database = format!("okapi_ttft_mean_upgrade_{}", Uuid::new_v4().simple());
        let env = setup_with_ch_database(&database).await;
        let ch = env
            .state
            .ch
            .as_ref()
            .expect("TTFT integration requires ClickHouse");
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
    if legacy_dimensions {
        ch.execute("DROP TABLE mv_analysis_hour SYNC")
            .await
            .unwrap();
    }
    ch.execute("DROP TABLE mv_ttft_reporting_hour SYNC")
        .await
        .unwrap();
    let mut legacy = vec![
        row(env, json!(1000), true),
        row(env, json!(9000), false),
        row(env, Value::Null, true),
    ];
    // Old zero means unknown. The NULL receipt is what pre-upgrade raw rows contain.
    for record in &mut legacy {
        record["ttft_reported"] = Value::Null;
    }
    insert(ch, &legacy).await;
    ch.ensure_schema().await.unwrap();
    insert(ch, &[row(env, json!(0), true)]).await;
    for total in totals(env).await {
        assert_average(&total, 4, 2, 1000, 4);
    }
    ch.execute("ALTER TABLE request_log_raw DELETE WHERE ttft_reported IS NULL SETTINGS mutations_sync = 1").await.unwrap();
    for total in totals(env).await {
        assert_average(&total, 4, 1, 0, 1);
        assert_eq!(total["ttft_history_coverage_bp"], 2500);
        assert_eq!(total["tokens"], 1200);
        assert_eq!(total["amount_micro"], 4000);
    }
    let query = format!("days=2&user_id={}", env.user_id);
    let folded = request(
        env,
        &format!("/admin/stats/breakdown?{query}&by=provider"),
        false,
    )
    .await;
    assert_average(&folded["data"][0], 4, 1, 0, 1);
    let stacked = request(
        env,
        &format!("/admin/stats/trend?{query}&stack=model"),
        false,
    )
    .await;
    assert_average(&stacked["data"][0]["values"][&env.model], 4, 1, 0, 1);
}

#[tokio::test]
async fn average_ttft_pg_summary_includes_measured_failures_zero_and_integer_rounding() {
    let env = setup().await;
    let (_, me) = get(&env, "/api/me", &env.user_token).await;
    let key = me["key_id"].as_i64().unwrap();
    for (status, ttft, stream) in [
        (20_i16, Some(0_i32), true),
        (40, Some(1001), true),
        (30, Some(2999), true),
        (20, Some(9999), false),
        (40, None, true),
        (10, Some(80000), true),
    ] {
        sqlx::query("INSERT INTO billing_records (request_id,user_id,api_key_id,model_name,status,ttft_ms,is_stream) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(env.user_id).bind(key).bind(&env.model).bind(status).bind(ttft).bind(stream)
            .execute(&env.pg).await.unwrap();
    }
    let body = request(&env, "/api/me/logs/stat?scope=user", true).await;
    assert_eq!(body["records"], 6);
    assert_eq!(body["ttft_samples"], 3);
    assert_eq!(body["avg_ttft_ms"], 1333);
}
