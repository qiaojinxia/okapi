//! Tools must retain observations and financial fee components after raw expiry.
use okapi_store::ChClient;
use okapi_store::ch::server_tools::{
    self, Breakdown, Filters, Granularity, ModelSource, StatisticsQuery,
};
use serde_json::json;
use uuid::Uuid;

fn query(user_id: i64) -> StatisticsQuery {
    StatisticsQuery {
        start: "2026-10-01".parse().unwrap(),
        end: "2026-10-01".parse().unwrap(),
        filters: Filters {
            user_id: Some(user_id),
            ..Filters::default()
        },
        model_source: ModelSource::Billed,
        granularity: Granularity::Day,
        by: Breakdown::Model,
        limit: 20,
        offset: 0,
    }
}

fn fees(amount: &serde_json::Value) -> String {
    let pricing = if amount == &json!(0) {
        json!({"billing":"included"})
    } else {
        json!({"billing":"additional","price_per_request_micro":amount})
    };
    json!({"server_tool_fees":[{"usage_contract":"anthropic_server_tool_use_v1","tool":"web_search","unit":"request","quantity":1,"requested":true,"pricing":pricing,"list_price_micro":amount,"amount_micro":amount,"original_amount_micro":amount,"discount_micro":0},{"usage_contract":"anthropic_server_tool_use_v1","tool":"web_fetch","unit":"request","quantity":null,"requested":false,"pricing":null,"list_price_micro":0,"amount_micro":0,"original_amount_micro":0,"discount_micro":0}]}).to_string()
}

fn event(log_type: u8, usage: &serde_json::Value, snapshot: String) -> serde_json::Value {
    json!({"ts":"2026-10-01 12:10:00.000","request_id":Uuid::new_v4(),"user_id":7,"api_key_id":8,"model":"tool-test","log_type":log_type,"server_tool_usage":usage.to_string(),"ratio_snapshot":serde_json::Value::String(snapshot)})
}

async fn fixture() -> (ChClient, String) {
    let database = format!("okapi_tools_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    (ch, database)
}

async fn close(ch: &ChClient, database: &str) {
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
}

async fn isolated<F, Fut>(check: F)
where
    F: FnOnce(ChClient) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (ch, database) = fixture().await;
    let result = tokio::spawn(check(ch.clone())).await;
    close(&ch, &database).await;
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

#[tokio::test]
async fn native_tools_and_signed_fees_survive_raw_expiry() {
    let database = format!("okapi_tool_red_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    let fee = json!({"usage_contract":"anthropic_server_tool_use_v1","tool":"web_search","unit":"request","quantity":2,"requested":true,"pricing":{"billing":"additional","price_per_request_micro":100},"list_price_micro":200,"amount_micro":200,"original_amount_micro":240,"discount_micro":40});
    let snapshot = json!({"server_tool_fees":[fee]}).to_string();
    let rows = [
        json!({"ts":"2026-10-01 12:00:00.000","request_id":Uuid::new_v4(),"log_type":2,"user_id":7,"api_key_id":8,"model":"native-tool","server_tool_usage":json!({"provider":"anthropic","web_search_requests":2}).to_string(),"ratio_snapshot":snapshot,"amount_micro":200,"original_amount_micro":240,"discount_micro":40}),
        json!({"ts":"2026-10-01 12:01:00.000","request_id":Uuid::new_v4(),"log_type":6,"user_id":7,"api_key_id":8,"model":"native-tool","ratio_snapshot":snapshot,"amount_micro":-200,"original_amount_micro":-240,"discount_micro":-40}),
    ];
    let token = Uuid::new_v4().to_string();
    ch.insert_json_each_row("request_log_raw", &rows, &token)
        .await
        .unwrap();
    ch.insert_json_each_row("request_log_raw", &rows, &token)
        .await
        .unwrap();
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    let result = ch.query_json_each_row("SELECT countMerge(calls) AS calls,countMerge(financial_records) AS records,sumMerge(web_search_quantity) AS quantity,countIfMerge(web_search_observed) AS observed,sumMerge(web_search_amount) AS amount FROM server_tool_minute_v1 WHERE user_id=7").await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    let values = result.unwrap();
    assert_eq!(
        values[0],
        json!({"calls":"1","records":"2","quantity":"2","observed":"1","amount":"0"})
    );
}

#[tokio::test]
async fn zero_missing_invalid_and_refund_have_independent_quantity_and_fee_coverage() {
    isolated(|ch| async move {
    let rows = [
        event(
            2,
            &json!({"provider":"anthropic","web_search_requests":2,"web_fetch_requests":0,"code_execution_requests":1}),
            fees(&json!(200)),
        ),
        event(
            5,
            &json!({"provider":"anthropic","web_search_requests":0}),
            fees(&json!(0)),
        ),
        event(
            2,
            &json!({"provider":"anthropic","web_search_requests":null}),
            fees(&json!(0)),
        ),
        event(
            2,
            &json!({"provider":"anthropic","web_search_requests":"2"}),
            fees(&json!(0)),
        ),
        event(
            2,
            &json!({"provider":"anthropic","web_search_requests":-1}),
            fees(&json!(0)),
        ),
        event(
            2,
            &json!({"provider":"anthropic","web_search_requests":1.5}),
            fees(&json!(0)),
        ),
        event(
            2,
            &json!({"provider":"anthropic","web_search_requests":2_147_483_648_u64}),
            fees(&json!(0)),
        ),
        event(
            2,
            &json!({"provider":"openai","web_search_requests":8}),
            fees(&json!(0)),
        ),
        event(
            6,
            &json!({"provider":"anthropic","web_search_requests":900}),
            fees(&json!(200)),
        ),
    ];
    ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
        .await
        .unwrap();
    for _ in 0..2 {
        let result = server_tools::read(&ch, &query(7)).await.unwrap();
        let t = &result["total"];
        assert_eq!(t["calls"], 8);
        assert_eq!(t["financial_records"], 9);
        let search = &t["tools"]["web_search"];
        assert_eq!(search["observed_quantity"], 2);
        assert_eq!(search["observed_calls"], 2);
        assert_eq!(search["coverage_bp"], 2500);
        assert!(search["quantity"].is_null());
        assert_eq!(search["fee_observed_records"], 9);
        assert_eq!(search["fee"]["amount_micro"], 0);
        assert_eq!(search["fee_complete"], true);
        assert_eq!(t["tools"]["web_fetch"]["observed_calls"], 1);
        assert_eq!(t["tools"]["web_fetch"]["observed_quantity"], 0);
        assert_eq!(t["tools"]["web_fetch"]["fee_complete"], true);
        let code = &t["tools"]["code_execution"];
        assert_eq!(code["observed_quantity"], 1);
        assert_eq!(code["billing_unit"], "container_duration");
        assert!(code["fee"].is_null());
        assert_eq!(code["fee_coverage_bp"], 0);
        assert_eq!(t["history"]["retained_records"], 9);
        assert_eq!(result["total_rows"], 1);
        assert_eq!(result["data"][0]["calls"], 8);
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    }
    }).await;
}

#[tokio::test]
async fn upgrade_recovers_only_one_source_and_partial_history_keeps_unknown_denominators() {
    isolated(|ch| async move {
        let row = event(
            2,
            &json!({"provider":"anthropic","web_search_requests":3}),
            fees(&json!(30)),
        );
        ch.insert_json_each_row("request_log_raw", &[row], &Uuid::new_v4().to_string())
            .await
            .unwrap();
        ch.execute("TRUNCATE TABLE server_tool_minute_v1")
            .await
            .unwrap();
        let recovered = server_tools::read(&ch, &query(7)).await.unwrap();
        assert_eq!(recovered["total"]["calls"], 1);
        assert_eq!(recovered["total"]["tools"]["web_search"]["quantity"], 3);
        assert_eq!(recovered["total"]["history"]["raw_recovered_records"], 1);
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
        let new = event(
            2,
            &json!({"provider":"anthropic","web_search_requests":2}),
            fees(&json!(20)),
        );
        let token = Uuid::new_v4().to_string();
        for _ in 0..2 {
            ch.insert_json_each_row("request_log_raw", std::slice::from_ref(&new), &token)
                .await
                .unwrap();
        }
        for _ in 0..2 {
            let partial = server_tools::read(&ch, &query(7)).await.unwrap();
            let t = &partial["total"];
            assert_eq!(t["calls"], 2);
            assert_eq!(t["financial_records"], 2);
            assert_eq!(t["tools"]["web_search"]["observed_quantity"], 2);
            assert!(t["tools"]["web_search"]["quantity"].is_null());
            assert_eq!(t["tools"]["web_search"]["coverage_bp"], 5000);
            assert!(t["tools"]["web_search"]["fee"].is_null());
            assert_eq!(t["tools"]["web_search"]["observed_fee"]["amount_micro"], 20);
            assert_eq!(t["history"]["missing_records"], 1);
            assert_eq!(t["history"]["retained_records"], 1);
            assert_eq!(t["history"]["raw_recovered_records"], 0);
            ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn duplicate_malformed_and_overflowed_fees_cannot_be_reported_as_known() {
    isolated(|ch| async move {
        let mut duplicate: serde_json::Value = serde_json::from_str(&fees(&json!(10))).unwrap();
        let extra = duplicate["server_tool_fees"][0].clone();
        duplicate["server_tool_fees"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        let rows = [
            event(
                2,
                &json!({"provider":"anthropic","web_search_requests":0}),
                duplicate.to_string(),
            ),
            event(
                2,
                &json!({"provider":"anthropic","web_search_requests":0}),
                fees(&json!("10")),
            ),
            event(
                2,
                &json!({"provider":"anthropic","web_search_requests":0}),
                fees(&json!(-1)),
            ),
        ];
        ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
            .await
            .unwrap();
        let result = server_tools::read(&ch, &query(7)).await.unwrap();
        let search = &result["total"]["tools"]["web_search"];
        assert_eq!(search["quantity"], 0);
        assert_eq!(search["fee_observed_records"], 0);
        assert!(search["observed_fee"].is_null());
        assert!(search["fee"].is_null());
        for _ in 0..2 {
            let row = event(
                2,
                &json!({"provider":"anthropic","web_search_requests":0}),
                fees(&json!(i64::MAX)),
            );
            ch.insert_json_each_row("request_log_raw", &[row], &Uuid::new_v4().to_string())
                .await
                .unwrap();
        }
        let result = server_tools::read(&ch, &query(7)).await;
        assert!(
            matches!(
                result,
                Err(okapi_store::StoreError::InvalidData(
                    "statistics_tool_data_invalid"
                ))
            ),
            "{result:?}"
        );
    })
    .await;
}

fn surcharge() -> String {
    let mut value: serde_json::Value = serde_json::from_str(&fees(&json!(100))).unwrap();
    value["server_tool_fees"][0]["amount_micro"] = json!(200);
    value["server_tool_fees"][0]["discount_micro"] = json!(-100);
    value.to_string()
}

#[tokio::test]
async fn negative_discounts_are_known_but_invalid_signed_money_remains_unknown() {
    isolated(|ch| async move {
        let native = json!({"provider":"anthropic","web_search_requests":1});
        let good = surcharge();
        let mut rows = vec![
            event(2, &native, good.clone()),
            event(6, &native, good.clone()),
        ];
        for (field, value) in [
            ("amount_micro", json!(-1)),
            ("original_amount_micro", json!(-1)),
            ("list_price_micro", json!(-100)),
            ("discount_micro", json!("-100")),
            ("discount_micro", json!(-100.5)),
            ("discount_micro", json!(-99)),
        ] {
            let mut invalid: serde_json::Value = serde_json::from_str(&good).unwrap();
            invalid["server_tool_fees"][0][field] = value;
            rows.push(event(2, &native, invalid.to_string()));
        }
        rows.push(event(
            2,
            &native,
            good.replace(
                "\"discount_micro\":-100",
                "\"discount_micro\":-9223372036854775809",
            ),
        ));
        ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
            .await
            .unwrap();
        for _ in 0..2 {
            let result = server_tools::read(&ch, &query(7)).await.unwrap();
            let total = &result["total"];
            assert_eq!(total["calls"], 8);
            assert_eq!(total["financial_records"], 9);
            let search = &total["tools"]["web_search"];
            assert_eq!(search["quantity"], 8);
            assert_eq!(search["fee_observed_records"], 2);
            assert!(search["fee"].is_null());
            assert_eq!(
                search["observed_fee"],
                json!({"amount_micro":0,"original_amount_micro":0,"discount_micro":0})
            );
            assert_eq!(total["history"]["retained_records"], 9);
            ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
        }
    })
    .await;
}

async fn identity(ch: &ChClient) -> serde_json::Value {
    json!(ch.query_json_each_row("SELECT name,toString(uuid) AS uuid,engine FROM system.tables WHERE database=currentDatabase() AND (name='server_tool_minute_v1' OR name=(SELECT concat('.inner_id.',toString(uuid)) FROM system.tables WHERE database=currentDatabase() AND name='server_tool_minute_v1')) ORDER BY name").await.unwrap())
}

#[tokio::test]
async fn existing_fee_projection_upgrade_preserves_storage_and_only_recovers_available_raw() {
    isolated(legacy_upgrade).await;
}

async fn legacy_upgrade(ch: ChClient) {
    // Replace only the empty fixture view, before any legacy history exists.
    ch.execute("DROP TABLE server_tool_minute_v1 SYNC")
        .await
        .unwrap();
    ch.execute(include_str!(
        "fixtures/server_tool_minute_v1_nonnegative.sql"
    ))
    .await
    .unwrap();
    let native = json!({"provider":"anthropic","web_search_requests":1});
    let rows = [
        event(2, &native, fees(&json!(100))),
        event(2, &native, surcharge()),
    ];
    ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
        .await
        .unwrap();
    let before = identity(&ch).await;
    let (first, second) = tokio::join!(ch.ensure_schema(), ch.ensure_schema());
    first.unwrap();
    second.unwrap();
    assert_eq!(identity(&ch).await, before);
    let recovered = server_tools::read(&ch, &query(7)).await.unwrap();
    assert_eq!(recovered["total"]["calls"], 2);
    assert_eq!(recovered["total"]["tools"]["web_search"]["quantity"], 2);
    assert_eq!(recovered["total"]["history"]["raw_recovered_records"], 2);
    assert_eq!(
        recovered["total"]["tools"]["web_search"]["fee"],
        json!({"amount_micro":300,"original_amount_micro":200,"discount_micro":-100})
    );
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    let expired = server_tools::read(&ch, &query(7)).await.unwrap();
    assert_eq!(expired["total"]["tools"]["web_search"]["quantity"], 2);
    assert_eq!(expired["total"]["history"]["retained_records"], 2);
    assert_eq!(
        expired["total"]["tools"]["web_search"]["fee_observed_records"],
        1
    );
    assert!(expired["total"]["tools"]["web_search"]["fee"].is_null());
    assert_eq!(
        expired["total"]["tools"]["web_search"]["observed_fee"],
        json!({"amount_micro":100,"original_amount_micro":100,"discount_micro":0})
    );
    new_signed_history(&ch).await;
    ch.ensure_schema().await.unwrap();
    assert_eq!(identity(&ch).await, before);
}

async fn new_signed_history(ch: &ChClient) {
    let native = json!({"provider":"anthropic","web_search_requests":1});
    ch.insert_json_each_row(
        "request_log_raw",
        &[event(2, &native, surcharge())],
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    for _ in 0..2 {
        let result = server_tools::read(ch, &query(7)).await.unwrap();
        let total = &result["total"];
        let search = &total["tools"]["web_search"];
        assert_eq!(total["calls"], 3);
        assert_eq!(search["quantity"], 3);
        assert_eq!(search["fee_observed_records"], 2);
        assert_eq!(search["fee_coverage_bp"], 6666);
        assert!(search["fee"].is_null());
        assert_eq!(
            search["observed_fee"],
            json!({"amount_micro":300,"original_amount_micro":200,"discount_micro":-100})
        );
        assert_eq!(total["history"]["retained_records"], 3);
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    }
    ch.insert_json_each_row(
        "request_log_raw",
        &[event(6, &native, surcharge())],
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    let result = server_tools::read(ch, &query(7)).await.unwrap();
    let search = &result["total"]["tools"]["web_search"];
    assert_eq!(result["total"]["calls"], 3);
    assert_eq!(result["total"]["financial_records"], 4);
    assert_eq!(search["quantity"], 3);
    assert_eq!(search["fee_observed_records"], 3);
    assert_eq!(search["fee_coverage_bp"], 7500);
    assert!(search["fee"].is_null());
    assert_eq!(
        search["observed_fee"],
        json!({"amount_micro":100,"original_amount_micro":100,"discount_micro":0})
    );
}

#[tokio::test]
async fn minute_precision_keeps_local_midnight_and_dst_hours_after_raw_expiry() {
    use chrono::{Local, TimeZone, Utc};
    isolated(|ch| async move {
        let date = Local::now().date_naive();
        let midnight = (0..1440)
            .find_map(|minute| {
                Local
                    .from_local_datetime(&date.and_hms_opt(minute / 60, minute % 60, 0).unwrap())
                    .earliest()
            })
            .unwrap()
            .with_timezone(&Utc);
        let rows = [(-1, 1), (1, 2)].map(|(offset, n)| {
            let mut row = event(
                2,
                &json!({"provider":"anthropic","web_search_requests":n}),
                fees(&json!(0)),
            );
            row["ts"] = json!(
                (midnight + chrono::Duration::seconds(offset))
                    .format("%Y-%m-%d %H:%M:%S%.3f")
                    .to_string()
            );
            row
        });
        ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
            .await
            .unwrap();
        let mut q = query(7);
        q.start = date.pred_opt().unwrap();
        q.end = date;
        for _ in 0..2 {
            let result = server_tools::read(&ch, &q).await.unwrap();
            assert_eq!(result["total"]["tools"]["web_search"]["quantity"], 3);
            assert_eq!(result["data"].as_array().unwrap().len(), 2);
            assert_eq!(
                result["data"][0]["bucket"],
                date.pred_opt().unwrap().to_string()
            );
            assert_eq!(result["data"][0]["tools"]["web_search"]["quantity"], 1);
            assert_eq!(result["data"][1]["bucket"], date.to_string());
            assert_eq!(result["data"][1]["tools"]["web_search"]["quantity"], 2);
            let mut single = query(7);
            single.start = date;
            single.end = date;
            let local_day = server_tools::read(&ch, &single).await.unwrap();
            assert_eq!(local_day["total"]["calls"], 1);
            assert_eq!(local_day["total"]["tools"]["web_search"]["quantity"], 2);
            ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
        }
        ch.execute("TRUNCATE TABLE server_tool_minute_v1")
            .await
            .unwrap();
        q.start = date;
        let history = server_tools::read(&ch, &q).await;
        if midnight.timestamp().rem_euclid(3600) != 0 {
            assert!(
                matches!(
                    history,
                    Err(okapi_store::StoreError::InvalidData(
                        "statistics_calendar_history_incomplete"
                    ))
                ),
                "{history:?}"
            );
        } else {
            let history = history.unwrap();
            assert_eq!(history["total"]["calls"], 1);
            assert!(history["total"]["tools"]["web_search"]["quantity"].is_null());
        }
    })
    .await;
    isolated(|ch| async move {
        let rows = ["2025-11-02 08:30:00.000", "2025-11-02 09:30:00.000"].map(|ts| {
            let mut row = event(
                2,
                &json!({"provider":"anthropic","web_search_requests":1}),
                fees(&json!(0)),
            );
            row["ts"] = json!(ts);
            row
        });
        ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
            .await
            .unwrap();
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
        let mut q = query(7);
        q.start = "2025-11-02".parse().unwrap();
        q.end = q.start;
        q.granularity = Granularity::Hour;
        let result = server_tools::read(&ch, &q).await.unwrap();
        assert_eq!(result["total"]["calls"], 2);
        assert_eq!(result["data"].as_array().unwrap().len(), 2);
        assert_ne!(result["data"][0]["bucket"], result["data"][1]["bucket"]);
    })
    .await;
}

#[tokio::test]
async fn unknown_event_and_conflicting_sources_block_only_the_selected_scope() {
    isolated(|ch| async move {
        let mut outside = event(0, &json!({}), String::new());
        outside["user_id"] = json!(9);
        let row = event(
            2,
            &json!({"provider":"anthropic","web_search_requests":0}),
            fees(&json!(0)),
        );
        ch.insert_json_each_row(
            "request_log_raw",
            &[row, outside],
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
        assert_eq!(
            server_tools::read(&ch, &query(7)).await.unwrap()["total"]["calls"],
            1
        );
        let unknown = server_tools::read(&ch, &query(9)).await;
        assert!(
            matches!(
                unknown,
                Err(okapi_store::StoreError::InvalidData(
                    "statistics_request_history_incomplete"
                ))
            ),
            "{unknown:?}"
        );
        ch.execute(
            "INSERT INTO server_tool_minute_v1 SELECT * FROM server_tool_minute_v1 WHERE user_id=7",
        )
        .await
        .unwrap();
        let conflict = server_tools::read(&ch, &query(7)).await;
        assert!(
            matches!(
                conflict,
                Err(okapi_store::StoreError::InvalidData(
                    "statistics_request_history_incomplete"
                ))
            ),
            "{conflict:?}"
        );
    })
    .await;
}
