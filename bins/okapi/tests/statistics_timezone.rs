//! Calendar boundaries must follow the host even when database containers use UTC.
use chrono::{Local, TimeZone, Utc};
use futures::FutureExt;
use okapi_store::ChClient;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn local_midnight_and_retained_calendar_dimensions_agree() {
    okapi_store::test_support::assert_isolated();
    let database = format!("okapi_timezone_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check(&ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[allow(clippy::too_many_lines)]
async fn check(ch: &ChClient) {
    let date = Local::now().date_naive();
    // Some DST transitions skip midnight. Pick the first valid minute of this date.
    let midnight = (0..1440)
        .find_map(|minute| {
            Local
                .from_local_datetime(&date.and_hms_opt(minute / 60, minute % 60, 0).unwrap())
                .earliest()
        })
        .unwrap()
        .with_timezone(&Utc);
    let rows = [-1, 1].map(|offset| {
        let payload = json!({"ts":(midnight + chrono::Duration::seconds(offset)).format("%Y-%m-%d %H:%M:%S%.3f").to_string(),"request_id":Uuid::new_v4(),"user_id":1,"api_key_id":1,"group":"default","model":"timezone-model","log_type":2,"status":20,"amount_micro":100,"original_amount_micro":120,"discount_micro":20,"upstream_cost_micro":45,"prompt_tokens":10,"completion_tokens":5,"cached_tokens":2,"reasoning_tokens":1,"client_type":"test-client","cache_write_tokens":3,"cache_write_reported":true,"cache_read_reported":true});
        okapi::worker::chsink::js_payload_to_ch_row(&payload)
    });
    ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
        .await
        .unwrap();
    let calendar = ch
        .query_json_each_row("SELECT toString(today()) AS today, timezone() AS timezone")
        .await
        .unwrap();
    assert_eq!(calendar[0]["today"], date.to_string());
    assert_eq!(
        calendar[0]["timezone"],
        okapi_store::timezone::machine_timezone().unwrap()
    );
    for _ in 0..2 {
        let days = ch.query_json_each_row("SELECT toString(day) AS day,countMerge(requests) AS n,sumMerge(amount) AS amount FROM mv_user_day GROUP BY day ORDER BY day").await.unwrap();
        assert_eq!(days.len(), 2, "{days:?}");
        assert_eq!(days[0]["day"], date.pred_opt().unwrap().to_string());
        assert_eq!(days[1]["day"], date.to_string());
        assert_eq!(days[1]["n"].as_str().unwrap().parse::<i64>().unwrap(), 1);
        assert_eq!(days[1]["amount"], "100");
        let totals = ch.query_json_each_row("SELECT sumMerge(tokens) AS tokens,sumMerge(original) AS original,sumMerge(discount) AS discount,sumMerge(upstream_cost) AS cost FROM mv_user_day WHERE day=today()").await.unwrap();
        assert_eq!(
            totals[0],
            json!({"tokens":"15","original":"120","discount":"20","cost":"45"})
        );
        for table in [
            "mv_apikey_day",
            "mv_user_model_day",
            "mv_key_model_day",
            "mv_group_day",
        ] {
            let total = ch.query_json_each_row(&format!("SELECT countMerge(requests) AS n,sumMerge(amount) AS amount FROM {table} WHERE day=today()")).await.unwrap();
            assert_eq!(total[0], json!({"n":"1","amount":"100"}), "{table}");
        }
        let clients = ch
            .query_json_each_row(
                "SELECT countMerge(requests) AS n FROM mv_client_day WHERE day=today()",
            )
            .await
            .unwrap();
        assert_eq!(clients[0]["n"].as_str().unwrap().parse::<i64>().unwrap(), 1);
        let cache = ch
            .query_json_each_row(
                "SELECT sumMerge(write_tokens) AS n FROM mv_cache_write_day WHERE day=today()",
            )
            .await
            .unwrap();
        assert_eq!(cache[0]["n"].as_str().unwrap().parse::<i64>().unwrap(), 3);
        let observed = ch.query_json_each_row("SELECT countIfMerge(read_known) AS reads,countIfMerge(write_known) AS writes,sumMerge(write_tokens) AS tokens FROM mv_cache_reporting_day WHERE day=today()").await.unwrap();
        assert_eq!(observed[0], json!({"reads":"1","writes":"1","tokens":"3"}));
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    }
    let pg = okapi_store::connect_pg(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    let pg_day: String = sqlx::query_scalar("SELECT to_char($1::timestamptz,'YYYY-MM-DD')")
        .bind(midnight + chrono::Duration::seconds(1))
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(pg_day, date.to_string());
    truncate_source(ch, "mv_calendar_minute").await;
    if midnight.timestamp().rem_euclid(3600) != 0 {
        for table in [
            "mv_user_day",
            "mv_apikey_day",
            "mv_user_model_day",
            "mv_key_model_day",
            "mv_group_day",
            "mv_client_day",
            "mv_cache_write_day",
            "mv_cache_reporting_day",
        ] {
            let error = ch
                .query_json_each_row(&format!("SELECT * FROM {table} WHERE day=today()"))
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    okapi_store::StoreError::InvalidData("statistics_calendar_history_incomplete")
                ),
                "{table}: {error}"
            );
        }
    } else {
        let legacy = ch.query_json_each_row("SELECT countMerge(requests) AS n,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount FROM mv_user_day WHERE day=today()").await.unwrap();
        assert_eq!(legacy[0], json!({"n":"1","tokens":"15","amount":"100"}));
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn partial_minute_coverage_chooses_one_hour_source() {
    okapi_store::test_support::assert_isolated();
    let database = format!("okapi_timezone_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        let date = Local::now().date_naive();
        let noon = Local.from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap()).earliest().unwrap().with_timezone(&Utc);
        let row = |offset, user| okapi::worker::chsink::js_payload_to_ch_row(&json!({"ts":(noon+chrono::Duration::seconds(offset)).format("%Y-%m-%d %H:%M:%S%.3f").to_string(),"request_id":Uuid::new_v4(),"user_id":user,"api_key_id":1,"group":"default","model":"timezone-model","log_type":2,"status":20,"amount_micro":100,"original_amount_micro":120,"discount_micro":20,"upstream_cost_micro":45,"prompt_tokens":10,"completion_tokens":5,"client_type":"test-client","cache_write_tokens":3,"cache_write_reported":true,"cache_read_reported":true}));
        ch.insert_json_each_row("request_log_raw", &[row(1, 1), row(2, 2)], &Uuid::new_v4().to_string()).await.unwrap();
        truncate_source(&ch, "mv_calendar_minute").await;
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
        ch.insert_json_each_row("request_log_raw", &[row(3, 1)], &Uuid::new_v4().to_string()).await.unwrap();
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
        let totals = ch.query_json_each_row("SELECT countMerge(requests) AS n,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount,sumMerge(original) AS original,sumMerge(discount) AS discount,sumMerge(upstream_cost) AS cost FROM mv_user_day WHERE day=today()").await.unwrap();
        assert_eq!(totals[0], json!({"n":"3","tokens":"45","amount":"300","original":"360","discount":"60","cost":"135"}));
        let clients = ch.query_json_each_row("SELECT countMerge(requests) AS n,uniqMerge(users) AS users FROM mv_client_day WHERE day=today()").await.unwrap();
        assert_eq!(clients[0], json!({"n":"3","users":"2"}));
        let cache = ch.query_json_each_row("SELECT sumMerge(write_tokens) AS tokens,countIfMerge(known_requests) AS known FROM mv_cache_write_day WHERE day=today()").await.unwrap();
        assert_eq!(cache[0], json!({"tokens":"9","known":"3"}));
    }).catch_unwind().await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn retained_day_only_history_is_not_reported_as_zero() {
    okapi_store::test_support::assert_isolated();
    let database = format!("okapi_timezone_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_day_only(&ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

const DAY_HISTORY_QUERIES: [(&str, &str, &str, &str); 8] = [
    (
        "mv_user_day",
        "countMerge(requests)",
        "user_id=7",
        "user_id=8",
    ),
    (
        "mv_apikey_day",
        "countMerge(requests)",
        "api_key_id=7",
        "api_key_id=8",
    ),
    (
        "mv_user_model_day",
        "countMerge(requests)",
        "user_id=7",
        "user_id=8",
    ),
    (
        "mv_key_model_day",
        "countMerge(requests)",
        "user_id=7",
        "user_id=8",
    ),
    (
        "mv_group_day",
        "countMerge(requests)",
        "group_code='history-group'",
        "group_code='other-group'",
    ),
    (
        "mv_client_day",
        "countMerge(requests)",
        "client_type='history-client'",
        "client_type='other-client'",
    ),
    (
        "mv_cache_write_day",
        "countIfMerge(known_requests)",
        "user_id=7",
        "user_id=8",
    ),
    (
        "mv_cache_reporting_day",
        "countIfMerge(read_known)",
        "user_id=7",
        "user_id=8",
    ),
];

async fn check_day_only(ch: &ChClient) {
    let payload = json!({"ts":Utc::now().format("%Y-%m-%d 12:00:00.000").to_string(),"request_id":Uuid::new_v4(),"user_id":7,"api_key_id":7,"group":"history-group","model":"history-model","log_type":2,"status":20,"amount_micro":100,"original_amount_micro":120,"discount_micro":20,"upstream_cost_micro":45,"prompt_tokens":10,"completion_tokens":5,"client_type":"history-client","cache_write_tokens":3,"cache_write_reported":true,"cache_read_reported":true});
    ch.insert_json_each_row(
        "request_log_raw",
        &[okapi::worker::chsink::js_payload_to_ch_row(&payload)],
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    for table in [
        "request_log_raw",
        "mv_cube_hour",
        "mv_calendar_minute",
        "mv_calendar_client_hour",
        "mv_calendar_cache_write_hour",
        "mv_calendar_cache_reporting_hour",
    ] {
        truncate_source(ch, table).await;
    }
    let utc = matches!(
        okapi_store::timezone::machine_timezone().unwrap(),
        "UTC" | "Etc/UTC"
    );
    for (table, measure, scope, other) in DAY_HISTORY_QUERIES {
        let query = format!("SELECT {measure} AS n FROM {table} WHERE {scope}");
        let result = ch.query_json_each_row(&query).await;
        if utc {
            assert_eq!(result.unwrap()[0]["n"], "1", "{table}");
        } else {
            assert!(
                matches!(
                    result,
                    Err(okapi_store::StoreError::InvalidData(
                        "statistics_calendar_history_incomplete"
                    ))
                ),
                "{table}: {result:?}"
            );
        }
        for predicate in [
            other.to_owned(),
            format!("{scope} AND day=toDate('2099-01-01')"),
        ] {
            let control = ch
                .query_json_each_row(&format!(
                    "SELECT {measure} AS n FROM {table} WHERE {predicate}"
                ))
                .await
                .unwrap();
            assert_eq!(control[0]["n"], "0", "{table}: {predicate}");
        }
    }
}

// A calendar source now has old record evidence and classified states. Removing
// only one leaves usable history; remove both to keep this precision fixture real.
async fn truncate_source(ch: &ChClient, table: &str) {
    ch.execute(&format!("TRUNCATE TABLE {table}"))
        .await
        .unwrap();
    if table.starts_with("mv_") {
        ch.execute(&format!("TRUNCATE TABLE population_v1_{table}"))
            .await
            .unwrap();
    }
}
