use super::token_detail_aggregates::{key_id, request, sample};
use super::ttft_statistics::insert;
use super::{Env, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn portal_complete_cache_sources_survive_missing_calendar_upgrades() {
    let database = format!("okapi_cache_upgrade_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_calendar_upgrade(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_calendar_upgrade(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let first = sample(env, key, Some(true), true);
    let mut second = first.clone();
    second["model"] = json!(format!("{}-second", env.model));
    second["cache_write_tokens"] = json!(200);
    insert(ch, &[first, second]).await;
    for table in [
        "mv_calendar_minute",
        "mv_calendar_cache_write_hour",
        "mv_calendar_cache_reporting_hour",
    ] {
        super::population_storage::execute(ch, &format!("TRUNCATE TABLE {table}"))
            .await
            .unwrap();
    }
    if !matches!(
        okapi_store::timezone::machine_timezone().unwrap(),
        "UTC" | "Etc/UTC"
    ) {
        assert!(matches!(
            ch.query_json_each_row("SELECT sumMerge(write_tokens) FROM mv_cache_write_day")
                .await,
            Err(okapi_store::StoreError::InvalidData(
                "statistics_calendar_history_incomplete"
            ))
        ));
    }
    for phase in 0..3 {
        if phase == 1 {
            // Equal overall counts can still have invalid per-model coverage.
            super::population_storage::execute(ch,&format!(
                "ALTER TABLE mv_cache_totals_5min DELETE WHERE model='{}-second' SETTINGS mutations_sync=2",
                env.model
            ))
            .await
            .unwrap();
            super::population_storage::execute(
                ch,
                "INSERT INTO mv_cache_totals_5min SELECT * FROM mv_cache_totals_5min",
            )
            .await
            .unwrap();
        } else if phase == 2 {
            super::population_storage::execute(ch, "TRUNCATE TABLE mv_cache_totals_5min")
                .await
                .unwrap();
        }
        for scope in ["key", "user"] {
            let body = request(
                env,
                &format!("/api/me/stats/breakdown?days=2&scope={scope}"),
                true,
            )
            .await;
            assert_cache(&body["total"], 2, 2, Some(300));
            assert_eq!(body["total"]["cache_read_known_requests"], 2);
            assert_eq!(body["total"]["amount_micro"], 2000);
            assert_eq!(body["total"]["tokens"], 3000);
            for row in body["data"].as_array().unwrap() {
                assert_cache(
                    row,
                    1,
                    1,
                    Some(if row["model"] == env.model { 100 } else { 200 }),
                );
            }
        }
    }
    if !matches!(
        okapi_store::timezone::machine_timezone().unwrap(),
        "UTC" | "Etc/UTC"
    ) {
        super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
            .await
            .unwrap();
        let (status, body) = super::get(
            env,
            "/api/me/stats/breakdown?days=2&scope=user",
            &env.user_token,
        )
        .await;
        assert_eq!(
            status, 500,
            "Incomplete calendar history must not be guessed"
        );
        assert_eq!(
            body["error"]["param"],
            "statistics_calendar_history_incomplete"
        );
    }
}

#[tokio::test]
async fn retained_day_cache_totals_do_not_leak_into_hours_or_channel_filters() {
    let database = format!("okapi_cache_days_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_days(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_days(env: &Env, ch: &ChClient) {
    let (key, day) = insert_day_history(env, ch).await;
    let path = format!(
        "/admin/stats/trend?user_id={}&start_date={day}&end_date={day}",
        env.user_id
    );
    let raw = request(env, &path, false).await;
    assert_cache(&raw["total"], 3, 3, Some(200));
    super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
        .await
        .unwrap();
    let expired = request(env, &path, false).await;
    assert_cache(&expired["total"], 3, 3, Some(200));
    assert_eq!(expired["total"]["tokens"], 4500);
    assert_eq!(expired["total"]["amount_micro"], 3000);
    assert!(
        expired["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["cache_write_tokens"].is_null())
    );
    let daily = request(env, &format!("{path}&granularity=day"), false).await;
    assert_cache(&daily["data"][0], 3, 3, Some(200));
    let stacked = request(env, &format!("{path}&stack=model&granularity=day"), false).await;
    assert_cache(&stacked["total"], 3, 3, Some(200));
    assert_cache(&stacked["data"][0]["values"][&env.model], 3, 3, Some(200));
    for by in ["model", "user", "api_key"] {
        let body = request(
            env,
            &format!(
                "/admin/stats/breakdown?by={by}&user_id={}&start_date={day}&end_date={day}",
                env.user_id
            ),
            false,
        )
        .await;
        assert_cache(&body["data"][0], 3, 3, Some(200));
    }
    let provider = request(
        env,
        &format!(
            "/admin/stats/breakdown?by=provider&user_id={}&start_date={day}&end_date={day}",
            env.user_id
        ),
        false,
    )
    .await;
    assert_eq!(provider["data"].as_array().unwrap().len(), 1);
    assert_cache(&provider["data"][0], 3, 3, Some(200));
    check_day_filters(env, &path, &day, key).await;
    let filtered = request(env, &format!("{path}&channel_id={}", env.channel_id), false).await;
    assert_cache(&filtered["total"], 2, 1, None);
    let grouped = request(env, &format!("{path}&group=another-group"), false).await;
    assert_cache(&grouped["total"], 1, 0, None);
    let flow = request(
        env,
        &format!(
            "/admin/stats/flow?user_id={}&start_date={day}&end_date={day}",
            env.user_id
        ),
        false,
    )
    .await;
    assert_cache(&flow["metrics"], 3, 3, Some(200));
    let portal = request(
        env,
        &format!("/api/me/stats/breakdown?scope=user&start_date={day}&end_date={day}"),
        true,
    )
    .await;
    assert_cache(&portal["total"], 3, 3, Some(200));
    assert_cache(&portal["data"][0], 3, 3, Some(200));
}

async fn check_day_filters(env: &Env, path: &str, day: &str, key: i64) {
    for suffix in [
        "model_source=requested",
        "model_source=upstream",
        "api_key_id",
    ] {
        let suffix = if suffix == "api_key_id" {
            format!("api_key_id={key}")
        } else {
            suffix.to_owned()
        };
        let body = request(env, &format!("{path}&{suffix}"), false).await;
        assert_cache(&body["total"], 3, 3, Some(200));
    }
    let node = request(env, &format!("{path}&node=test-node"), false).await;
    assert_cache(&node["total"], 1, 1, Some(0));
    let endpoints = request(
        env,
        &format!(
            "/admin/stats/breakdown?by=endpoint&user_id={}&start_date={day}&end_date={day}",
            env.user_id
        ),
        false,
    )
    .await;
    assert_eq!(
        endpoints["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["requests"].as_i64().unwrap())
            .sum::<i64>(),
        3
    );
}

async fn insert_day_history(env: &Env, ch: &ChClient) -> (i64, String) {
    let key = key_id(env).await;
    super::population_storage::execute(ch, "DROP TABLE mv_analysis_hour SYNC")
        .await
        .unwrap();
    super::population_storage::execute(ch, "DROP TABLE mv_cache_totals_5min SYNC")
        .await
        .unwrap();
    let mut old = sample(env, key, Some(true), false);
    // Stable historical day, away from UTC midnight; same parent across hours/channels.
    let day = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    old["ts"] = json!(format!("{day} 10:00:00.000"));
    let mut other = old.clone();
    other["ts"] = json!(format!("{day} 12:00:00.000"));
    let (other_channel, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("cache-{}", Uuid::new_v4().simple()),
        "openai",
        "http://127.0.0.1:1/v1",
        "mock",
        &[env.model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    other["channel_id"] = json!(other_channel);
    other["group_code"] = json!("another-group");
    insert(ch, &[old.clone(), other]).await;
    ch.ensure_schema().await.unwrap();
    let mut modern = old;
    modern["cache_write_tokens"] = json!(0);
    insert(ch, &[modern]).await;
    (key, day)
}

fn assert_cache(value: &Value, requests: i64, known: i64, tokens: Option<i64>) {
    assert_eq!(value["requests"], requests, "{value}");
    assert_eq!(value["cache_write_known_requests"], known, "{value}");
    assert_eq!(
        value["cache_write_tokens"],
        tokens.map_or(Value::Null, |n| json!(n)),
        "{value}"
    );
}

#[tokio::test]
async fn cache_samples_require_numbers_and_retained_counts_stay_in_scope() {
    let database = format!("okapi_cache_samples_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
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
    let key = key_id(env).await;
    let mut zero = sample(env, key, Some(true), true);
    zero["cache_write_tokens"] = json!(0);
    let mut missing = zero.clone();
    missing["cache_write_tokens"] = Value::Null;
    // A true flag without a numeric value must never create an observed zero.
    let mut unknown = zero.clone();
    unknown["cache_write_tokens"] = json!(999);
    unknown["cache_write_reported"] = Value::Null;
    unknown["cache_read_reported"] = Value::Null;
    let mut outside = zero.clone();
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["api_key_id"] = json!(key + 1_000_000);
    outside["cache_write_tokens"] = json!(9000);
    insert(ch, &[zero, missing, unknown, outside]).await;
    let path = format!("/admin/stats/trend?user_id={}&days=2", env.user_id);
    for expired in [false, true] {
        if expired {
            super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
                .await
                .unwrap();
        }
        let admin = request(env, &path, false).await;
        let portal = request(env, "/api/me/stats/breakdown?days=2&scope=user", true).await;
        assert_eq!(portal["total"]["recorded_cache_write_tokens"], 999);
        for row in portal["data"].as_array().unwrap() {
            assert_eq!(row["recorded_cache_write_tokens"], 999);
        }
        for body in [admin, portal] {
            assert_cache(&body["total"], 3, 1, None);
            assert_eq!(body["total"]["cache_read_known_requests"], 2);
        }
    }
    super::population_storage::execute(
        ch,
        "INSERT INTO mv_cache_totals_5min SELECT * FROM mv_cache_totals_5min",
    )
    .await
    .unwrap();
    // Invalid new counts must use an eligible old population, never double quantities.
    let corrupted = request(env, &path, false).await;
    assert_cache(&corrupted["total"], 3, 0, None);
    assert_eq!(corrupted["total"]["cache_read_known_requests"], 2);
}
