//! Refund events may carry copied timing metadata, but never become call samples.
use okapi_store::ChClient;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn reported_refund_timings_do_not_dilute_call_averages_after_retention() {
    let database = format!("okapi_refund_measurements_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    ch.execute(
        "INSERT INTO request_log_raw (ts,request_id,log_type,user_id,api_key_id,model,\
         stream,latency_ms,latency_reported,ttft_ms,ttft_reported,prompt_tokens,\
         completion_tokens,amount_micro,input_unit) VALUES \
         ('2026-09-30 12:00:00',generateUUIDv4(),2,7,8,'timed-call',1,9000,1,900,1,100,50,1000,'tokens'),\
         ('2026-09-30 12:05:00',generateUUIDv4(),6,7,8,'timed-call',1,1,1,1,1,0,0,-1000,'')",
    )
    .await
    .unwrap();

    for _ in 0..2 {
        for (table, expected_ms) in [
            ("mv_latency_reporting_hour", "9000"),
            ("mv_ttft_reporting_hour", "900"),
        ] {
            let result = ch
                .query_json_each_row(&format!(
                    "SELECT countMerge(requests) AS calls,countIfMerge(samples) AS samples,\
                     sumIfMerge(total_ms) AS ms FROM {table} WHERE user_id=7"
                ))
                .await
                .unwrap();
            assert_eq!(
                result[0],
                json!({"calls":"1","samples":"1","ms":expected_ms}),
                "{table}: refund must not contribute its copied measurement"
            );
        }
        let financial = ch
            .query_json_each_row(
                "SELECT countMerge(financial_records) AS records,countMerge(requests) AS calls,\
                 sumMerge(tokens) AS tokens,sumMerge(amount) AS amount FROM mv_user_day WHERE user_id=7",
            )
            .await
            .unwrap();
        assert_eq!(
            financial[0],
            json!({"records":"2","calls":"1","tokens":"150","amount":"0"})
        );
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    }
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}
