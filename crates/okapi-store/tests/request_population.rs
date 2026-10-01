//! Execute real ClickHouse states, including upgrade and retained-history paths.
use okapi_store::{ChClient, StoreError};
use serde_json::{Value, json};
use uuid::Uuid;

async fn fixture() -> (ChClient, String) {
    let database = format!("okapi_population_{}", Uuid::new_v4().simple());
    let client = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    client.ensure_schema().await.unwrap();
    (client, database)
}

async fn events(ch: &ChClient) {
    events_with_refund_date(ch, "2026-09-30 12:05:00").await;
}

async fn events_with_refund_date(ch: &ChClient, date: &str) {
    let sql = "INSERT INTO request_log_raw (ts,request_id,log_type,user_id,api_key_id,model,client_type,prompt_tokens,completion_tokens,amount_micro,original_amount_micro,discount_micro,stream,latency_ms,latency_reported,ttft_ms,ttft_reported,cache_read_reported,input_unit,upstream_prompt_tokens,upstream_completion_tokens,prompt_source,completion_source) VALUES \
        ('2026-09-30 12:00:00',generateUUIDv4(),2,7,8,'test-model','client',100,20,240,300,60,1,66,1,66,1,1,'tokens',100,20,'upstream','upstream'),\
        ('2026-09-30 12:05:00',generateUUIDv4(),6,7,8,'test-model','',0,0,-240,-300,-60,0,0,NULL,0,NULL,NULL,'',NULL,NULL,'unknown','unknown')";
    ch.execute(&sql.replace("2026-09-30 12:05:00", date))
        .await
        .unwrap();
}

async fn summary(ch: &ChClient) -> Value {
    ch.query_json_each_row("SELECT countMerge(requests) AS requests,countMerge(financial_records) AS records,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount,sumMerge(original) AS original,sumMerge(discount) AS discount FROM mv_user_day WHERE user_id=7")
        .await.unwrap().remove(0)
}

#[tokio::test]
async fn refund_changes_money_without_creating_calls_or_unknown_measurements() {
    let (ch, database) = fixture().await;
    events(&ch).await;
    for _ in 0..2 {
        assert_eq!(
            summary(&ch).await,
            json!({"requests":"1","records":"2","tokens":"120","amount":"0","original":"0","discount":"0"})
        );
        for table in [
            "mv_model_hour",
            "mv_cube_hour",
            "mv_analysis_hour",
            "mv_calendar_minute",
        ] {
            let scope = if table == "mv_model_hour" {
                ""
            } else {
                " WHERE user_id=7"
            };
            let rows = ch.query_json_each_row(&format!("SELECT countMerge(requests) AS calls,countMerge(financial_records) AS records,sumMerge(amount) AS amount FROM {table}{scope}")).await.unwrap();
            assert_eq!(
                rows[0],
                json!({"calls":"1","records":"2","amount":"0"}),
                "{table}"
            );
        }
        let measured = ch.query_json_each_row("SELECT countMerge(requests) AS calls,countIfMerge(samples) AS samples,sumIfMerge(total_ms) AS ms FROM mv_latency_reporting_hour WHERE user_id=7").await.unwrap();
        assert_eq!(measured[0], json!({"calls":"1","samples":"1","ms":"66"}));
        let units = ch.query_json_each_row("SELECT countMerge(requests) AS calls,countIfMerge(unit_token_n) AS known FROM mv_input_units_5min WHERE user_id=7").await.unwrap();
        assert_eq!(units[0], json!({"calls":"1","known":"1"}));
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    }
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}

#[tokio::test]
async fn unclassified_upgrade_recovers_only_complete_raw_history() {
    let (ch, database) = fixture().await;
    events(&ch).await;
    // Simulate installation after both financial records were already written.
    ch.execute("TRUNCATE TABLE population_v1_mv_user_day")
        .await
        .unwrap();
    assert_eq!(summary(&ch).await["requests"], "1");
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    let error = ch
        .query_json_each_row(
            "SELECT countMerge(requests) AS calls FROM mv_user_day WHERE user_id=7",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::InvalidData("statistics_request_history_incomplete")
        ),
        "{error}"
    );
    let finance = ch.query_json_each_row("SELECT sumMerge(amount) AS amount,countMerge(financial_records) AS records FROM mv_user_day WHERE user_id=7").await.unwrap();
    assert_eq!(finance[0], json!({"amount":"0","records":"2"}));
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}

#[tokio::test]
async fn cross_day_refund_keeps_original_use_and_adjustment_dates() {
    let (ch, database) = fixture().await;
    events_with_refund_date(&ch, "2026-10-01 12:05:00").await;
    for _ in 0..2 {
        let rows = ch.query_json_each_row("SELECT day,countMerge(requests) AS calls,countMerge(financial_records) AS records,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount FROM mv_user_day WHERE user_id=7 GROUP BY day ORDER BY day").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["calls"], "1");
        assert_eq!(rows[0]["tokens"], "120");
        assert_eq!(rows[0]["amount"], "240");
        assert_eq!(rows[1]["calls"], "0");
        assert_eq!(rows[1]["tokens"], "0");
        assert_eq!(rows[1]["amount"], "-240");
        assert_eq!(rows[1]["records"], "1");
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    }
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}

#[tokio::test]
async fn failure_is_a_call_but_refund_is_not_a_latency_sample() {
    let (ch, database) = fixture().await;
    events(&ch).await;
    ch.execute("INSERT INTO request_log_raw (ts,request_id,log_type,user_id,api_key_id,model,is_error,latency_ms,latency_reported,input_unit) VALUES ('2026-09-30 12:10:00',generateUUIDv4(),5,7,8,'test-model',1,77,1,'tokens')").await.unwrap();
    let summary = summary(&ch).await;
    assert_eq!(summary["requests"], "2");
    assert_eq!(summary["records"], "3");
    assert_eq!(summary["tokens"], "120");
    assert_eq!(summary["amount"], "0");
    let measured=ch.query_json_each_row("SELECT countMerge(requests) AS calls,countIfMerge(samples) AS samples,sumIfMerge(total_ms) AS ms FROM mv_latency_reporting_hour WHERE user_id=7").await.unwrap();
    assert_eq!(measured[0], json!({"calls":"2","samples":"2","ms":"143"}));
    let errors = ch
        .query_json_each_row("SELECT sumMerge(errors) AS errors FROM mv_user_day WHERE user_id=7")
        .await
        .unwrap();
    assert_eq!(errors[0]["errors"], "1");
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}

#[tokio::test]
async fn unknown_event_type_never_becomes_a_zero_or_an_extra_call() {
    let (ch, database) = fixture().await;
    ch.execute("INSERT INTO request_log_raw (ts,request_id,log_type,user_id,amount_micro) VALUES ('2026-09-30 12:00:00',generateUUIDv4(),0,7,100)").await.unwrap();
    let error = ch
        .query_json_each_row(
            "SELECT countMerge(requests) AS calls FROM mv_user_day WHERE user_id=7",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::InvalidData("statistics_request_history_incomplete")
        ),
        "{error}"
    );
    let money = ch
        .query_json_each_row("SELECT sumMerge(amount) AS amount FROM mv_user_day WHERE user_id=7")
        .await
        .unwrap();
    assert_eq!(money[0]["amount"], "100");
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}

#[tokio::test]
async fn partial_classified_history_never_adds_overlapping_records() {
    let (ch, database) = fixture().await;
    events(&ch).await;
    ch.execute("TRUNCATE TABLE population_v1_mv_user_day")
        .await
        .unwrap();
    ch.execute("INSERT INTO population_v1_mv_user_day SELECT user_id,toDate(ts) AS day,countStateIf(log_type IN (2,5)) AS requests,sumStateIf(toUInt64(prompt_tokens)+toUInt64(completion_tokens),log_type IN (2,5)) AS tokens,sumState(amount_micro) AS amount,sumState(original_amount_micro) AS original,sumState(discount_micro) AS discount,sumState(upstream_cost_micro) AS upstream_cost,sumStateIf(toUInt64(is_error),log_type IN (2,5)) AS errors,countState() AS financial_records,countStateIf(log_type IN (2,5,6)) AS population_classified FROM request_log_raw WHERE log_type=2 GROUP BY user_id,day").await.unwrap();
    assert_eq!(
        summary(&ch).await,
        json!({"requests":"1","records":"2","tokens":"120","amount":"0","original":"0","discount":"0"})
    );
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    let error = ch
        .query_json_each_row(
            "SELECT countMerge(requests) AS calls FROM mv_user_day WHERE user_id=7",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::InvalidData("statistics_request_history_incomplete")
        ),
        "{error}"
    );
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}
