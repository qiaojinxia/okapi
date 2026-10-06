use super::{Env, get, payload, setup_with_ch_database};
use futures::FutureExt as _;
use okapi::worker::chsink;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

fn snapshot(quantity: u32) -> String {
    let amount = i64::from(quantity) * 50;
    json!({"epoch":1,"server_tool_fees":[
        {"usage_contract":"anthropic_server_tool_use_v1","tool":"web_search","unit":"request","quantity":quantity,"requested":true,"pricing":{"billing":"additional","price_per_request_micro":50},"list_price_micro":amount,"amount_micro":amount,"original_amount_micro":amount,"discount_micro":0},
        {"usage_contract":"anthropic_server_tool_use_v1","tool":"web_fetch","unit":"request","quantity":null,"requested":true,"pricing":{"billing":"included"},"list_price_micro":0,"amount_micro":0,"original_amount_micro":0,"discount_micro":0}
    ]}).to_string()
}

fn row(env: &Env, quantity: Option<u32>, log_type: u8) -> Value {
    let mut value = chsink::js_payload_to_ch_row(&payload(env, &env.model_a, 1000, false));
    value["ts"] = json!(
        (chrono::Utc::now() - chrono::Duration::hours(2))
            .format("%Y-%m-%d %H:10:00.000")
            .to_string()
    );
    value["log_type"] = json!(log_type);
    value["requested_model"] = json!("client-tool-model");
    value["upstream_model"] = json!("vendor-tool-model");
    value["endpoint"] = json!("/v1/messages");
    value["upstream_endpoint"] = json!("/v1/messages");
    value["request_type"] = json!("stream");
    value["billing_type"] = json!("wallet");
    if let Some(n) = quantity {
        value["server_tool_usage"]=json!(json!({"provider":"anthropic","web_search_requests":n,"web_fetch_requests":0,"code_execution_requests":1}).to_string());
        value["ratio_snapshot"] = json!(snapshot(n));
    }
    if log_type == 6 {
        value["prompt_tokens"] = json!(0);
        value["completion_tokens"] = json!(0);
        value["cached_tokens"] = json!(0);
        value["reasoning_tokens"] = json!(0);
        for name in ["amount_micro", "original_amount_micro", "discount_micro"] {
            value[name] = json!(-value[name].as_i64().unwrap());
        }
    }
    value
}

async fn insert(ch: &ChClient, rows: &[Value]) {
    let token = Uuid::new_v4().to_string();
    for _ in 0..2 {
        ch.insert_json_each_row("request_log_raw", rows, &token)
            .await
            .unwrap();
    }
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

async fn isolated(kind: u8) {
    let database = format!("okapi_tool_http_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().unwrap();
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        match kind {
            0 => coverage(&env, ch).await,
            1 => scope_and_validation(&env, ch).await,
            2 => pagination(&env, ch).await,
            _ => unreachable!(),
        }
    })
    .catch_unwind()
    .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn coverage(env: &Env, ch: &ChClient) {
    let mut refund = row(env, Some(2), 6);
    refund["server_tool_usage"] =
        json!(json!({"provider":"anthropic","web_search_requests":999}).to_string());
    let mut outside = row(env, Some(4000), 2);
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["api_key_id"] = json!(env.key_id + 1_000_000);
    insert(
        ch,
        &[
            row(env, Some(2), 2),
            row(env, Some(0), 5),
            row(env, None, 2),
            refund,
            outside,
        ],
    )
    .await;
    for _ in 0..2 {
        for (path, portal) in [
            (
                format!("/admin/stats/tools?user_id={}&days=2", env.user_id),
                false,
            ),
            ("/api/me/stats/tools?days=2".to_owned(), true),
            ("/api/me/stats/tools?days=2&scope=user".to_owned(), true),
        ] {
            let result = request(env, &path, portal).await;
            let total = &result["total"];
            assert_eq!(total["calls"], 3, "{result}");
            assert_eq!(total["financial_records"], 4);
            assert_eq!(total["tools"]["web_search"]["observed_calls"], 2);
            assert_eq!(total["tools"]["web_search"]["observed_quantity"], 2);
            assert_eq!(total["tools"]["web_search"]["coverage_bp"], 6666);
            assert!(total["tools"]["web_search"]["quantity"].is_null());
            assert_eq!(
                total["tools"]["web_search"]["observed_fee"]["amount_micro"],
                0
            );
            assert_eq!(total["tools"]["web_search"]["fee_observed_records"], 3);
            assert_eq!(total["tools"]["web_search"]["fee_coverage_bp"], 7500);
            assert!(total["tools"]["web_search"]["fee"].is_null());
            assert_eq!(total["tools"]["code_execution"]["observed_quantity"], 2);
            assert!(total["tools"]["code_execution"]["fee"].is_null());
            assert_eq!(total["history"]["coverage_bp"], 10000);
            assert_eq!(result["tokens_included"], false);
        }
        let trend = request(
            env,
            &format!(
                "/admin/stats/trend?user_id={}&days=2&fields=core",
                env.user_id
            ),
            false,
        )
        .await;
        assert_eq!(trend["total"]["requests"], 3);
        assert_eq!(trend["total"]["tokens"], 900);
        assert_eq!(trend["total"]["amount_micro"], 2000);
        ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    }
}

async fn scope_and_validation(env: &Env, ch: &ChClient) {
    let mut another = row(env, Some(7), 2);
    another["api_key_id"] = json!(env.key_id + 500_000);
    let special = "model';\\N\tquoted";
    let mut first = row(env, Some(2), 2);
    first["requested_model"] = json!(special);
    insert(ch, &[first, another]).await;
    let default = request(env, "/api/me/stats/tools?days=2", true).await;
    assert_eq!(default["total"]["tools"]["web_search"]["quantity"], 2);
    let own = request(env, "/api/me/stats/tools?scope=user&days=2", true).await;
    assert_eq!(own["total"]["tools"]["web_search"]["quantity"], 9);
    let mut url = reqwest::Url::parse("http://unused/api/me/stats/tools").unwrap();
    url.query_pairs_mut()
        .append_pair("days", "2")
        .append_pair("model_source", "requested")
        .append_pair("model", special);
    let filtered = request(
        env,
        &format!("{}?{}", url.path(), url.query().unwrap()),
        true,
    )
    .await;
    assert_eq!(filtered["total"]["tools"]["web_search"]["quantity"], 2);
    assert_eq!(filtered["data"][0]["key"], special);
    for (path, expected) in [
        ("/admin/stats/tools?days=2".to_owned(), 403),
        (
            format!("/api/me/stats/tools?user_id={}", env.user_id + 1),
            403,
        ),
        (
            format!("/api/me/stats/tools?api_key_id={}", env.key_id + 1),
            403,
        ),
        ("/api/me/stats/tools?scope=all".to_owned(), 400),
        ("/api/me/stats/tools?model_source=guess".to_owned(), 400),
        ("/api/me/stats/tools?by=guess".to_owned(), 400),
        (
            "/api/me/stats/tools?granularity=hour&days=32".to_owned(),
            400,
        ),
        (
            "/api/me/stats/tools?start_date=2026-09-31&end_date=2026-10-01".to_owned(),
            400,
        ),
        ("/api/me/stats/tools?request_type=guess".to_owned(), 400),
        ("/api/me/stats/tools?channel_id=-1".to_owned(), 400),
        ("/api/me/stats/tools?offset=-1".to_owned(), 400),
    ] {
        let (status, body) = get(env, &path, &env.user_token).await;
        assert_eq!(status, expected, "{path}: {body}");
    }
    let (status, _) = get(env, "/api/me/stats/tools", "").await;
    assert_eq!(status, 401);
    for filters in [
        "endpoint=/v1/messages&stream=true&request_type=stream&billing_type=wallet",
        "model_source=upstream&model=vendor-tool-model",
        "node=test-node&group=default",
    ] {
        let result = request(env, &format!("/api/me/stats/tools?days=2&{filters}"), true).await;
        assert_eq!(result["total"]["calls"], 1, "{result}");
    }
    let empty = request(env, "/api/me/stats/tools?days=2&model=not-present", true).await;
    assert_eq!(empty["total"]["calls"], 0);
    assert_eq!(empty["total_rows"], 0);
    assert!(empty["total"]["tools"]["web_search"]["quantity"].is_null());
    assert!(empty["total"]["tools"]["web_search"]["coverage_bp"].is_null());
}

async fn pagination(env: &Env, ch: &ChClient) {
    let rows = (0..25)
        .map(|n| {
            let mut value = row(env, Some(1), 2);
            value["model"] = json!(format!("tool-page-{n:02}"));
            value
        })
        .collect::<Vec<_>>();
    insert(ch, &rows).await;
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    for (options, len) in [
        ("", 20),
        ("&offset=20", 5),
        ("&limit=500", 25),
        ("&offset=100", 0),
    ] {
        let result = request(env, &format!("/api/me/stats/tools?days=2{options}"), true).await;
        assert_eq!(result["data"].as_array().unwrap().len(), len);
        assert_eq!(result["total_rows"], 25);
        assert_eq!(result["total"]["calls"], 25);
        assert_eq!(result["total"]["tools"]["web_search"]["quantity"], 25);
        assert_eq!(
            result["total"]["tools"]["web_search"]["fee"]["amount_micro"],
            1250
        );
    }
}

#[tokio::test]
async fn native_counter_and_fee_coverage_survive_http_queries_and_raw_expiry() {
    isolated(0).await;
}

#[tokio::test]
async fn tool_statistics_bind_strings_and_enforce_owner_and_billing_permissions() {
    isolated(1).await;
}

#[tokio::test]
async fn tool_statistics_page_rows_in_backend_without_truncating_window_total() {
    isolated(2).await;
}
