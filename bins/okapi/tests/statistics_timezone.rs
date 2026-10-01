//! Calendar boundaries must follow the host even when database containers use UTC.
use chrono::{Local, TimeZone, Utc};
use futures::FutureExt;
use okapi_store::ChClient;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn local_midnight_and_retained_calendar_dimensions_agree() {
    dotenvy::dotenv().ok();
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

async fn check(ch: &ChClient) {
    let date = Local::now().date_naive();
    let midnight = Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .unwrap()
        .with_timezone(&Utc);
    let rows = [-1, 1].map(|offset| {
        let payload = json!({"ts":(midnight + chrono::Duration::seconds(offset)).format("%Y-%m-%d %H:%M:%S%.3f").to_string(),"request_id":Uuid::new_v4(),"user_id":1,"api_key_id":1,"group":"default","model":"timezone-model","log_type":2,"status":20,"amount_micro":100,"original_amount_micro":100,"prompt_tokens":10,"completion_tokens":5,"client_type":"test-client","cache_write_tokens":3,"cache_write_reported":true,"cache_read_reported":true});
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
}
