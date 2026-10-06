//! An unrequested tool is not an observed zero: monetary authority and usage differ.
use super::{
    Env, Protocol, anthropic_usage, record, report, request_with_tools, server_tool_fees, setup,
};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

fn fetch() -> Value {
    json!([{"type":"web_fetch_20250910","name":"web_fetch","max_uses":5}])
}

pub(super) async fn verify_published(
    env: &Env,
    id: &str,
    expected: &Value,
    snapshot: &Value,
    money: [i64; 4],
) {
    let event: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(event["server_tool_usage"], *expected);
    assert_eq!(event["upstream_cost_known"], true);
    for (field, value) in [
        ("amount_micro", money[0]),
        ("original_amount_micro", money[1]),
        ("discount_micro", money[2]),
        ("upstream_cost_micro", money[3]),
    ] {
        assert_eq!(event[field].as_i64().unwrap(), value);
    }
    assert_eq!(
        serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
        *snapshot
    );
    let ch = env.state.ch.as_ref().unwrap();
    ch.ensure_schema().await.unwrap();
    let mut records = Vec::new();
    for _ in 0..100 {
        okapi::worker::chsink::process_once(&env.state.pg, ch)
            .await
            .unwrap();
        records=ch.query_with_params("SELECT server_tool_usage,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,ratio_snapshot FROM request_log_raw WHERE request_id=toUUID({id:String})",&[("id",id)]).await.unwrap();
        if !records.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(records.len(), 1);
    let ch = &records[0];
    assert_eq!(
        serde_json::from_str::<Value>(ch["server_tool_usage"].as_str().unwrap()).unwrap(),
        *expected
    );
    assert_eq!(
        serde_json::from_str::<Value>(ch["ratio_snapshot"].as_str().unwrap()).unwrap(),
        *snapshot
    );
    for (field, value) in [
        ("amount_micro", money[0]),
        ("original_amount_micro", money[1]),
        ("discount_micro", money[2]),
        ("upstream_cost_micro", money[3]),
    ] {
        assert_eq!(
            ch[field]
                .as_i64()
                .unwrap_or_else(|| ch[field].as_str().unwrap().parse().unwrap()),
            value
        );
    }
}

async fn verify(env: &Env) {
    let expected = json!({"provider":"anthropic","web_fetch_requests":3});
    let row = record(env).await;
    assert_eq!(row["amount_micro"], 1600);
    assert_eq!(row["usage"]["server_tool_usage"], expected);
    assert_eq!(row["usage"]["prompt_tokens"], 1000);
    assert_eq!(row["usage"]["completion_tokens"], 50);
    let id = row["request_id"].as_str().unwrap();
    let pg: (i64, i64, i64, Option<i64>, Value, Value) = sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,usage_details FROM billing_records WHERE request_id::text=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!((pg.0, pg.1, pg.2, pg.3), (1600, 1600, 0, Some(2000)));
    assert_eq!(pg.5["tokens"]["server_tool_usage"], expected);
    let search = &pg.4["server_tool_fees"][0];
    assert!(search["quantity"].is_null());
    assert_eq!(search["requested"], false);
    assert_eq!(search["amount_micro"], 0);
    assert_eq!(search["pricing"]["price_per_request_micro"], 10000);
    assert_eq!(pg.4["server_tool_fees"][1]["requested"], true);
    assert_eq!(pg.4["server_tool_fees"][1]["quantity"], 3);
    verify_published(env, id, &expected, &pg.4, [1600, 1600, 0, 2000]).await;
    let stats = report(env, "/api/me/stats/breakdown?days=1").await;
    assert_eq!(stats["total"]["tokens"], 1050);
    assert_eq!(stats["total"]["amount_micro"], 1600);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - 1600
    );
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fetch_only_does_not_require_search_counter_or_invent_an_observed_zero() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_fetch_requests":3});
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, usage.clone()).await;
            server_tool_fees::activate(
                &env,
                &server_tool_fees::prices(
                    &json!({"billing":"additional","price_per_request_micro":10000}),
                ),
            )
            .await;
            let response = request_with_tools(
                &env,
                ingress,
                stream,
                matches!(ingress, Protocol::Gemini),
                Some(fetch()),
            )
            .await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{ingress:?}/{stream}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            verify(&env).await;
        }
    }
}

#[tokio::test]
async fn search_only_preserves_missing_fetch_quantity_and_its_nonfree_price() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2});
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, usage.clone()).await;
            let profile = json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":{"billing":"additional","price_per_request_micro":10000},"web_fetch":{"billing":"additional","price_per_request_micro":500}});
            let epoch = server_tool_fees::activate(&env, &profile).await;
            let response = request_with_tools(
                &env,
                ingress,
                stream,
                matches!(ingress, Protocol::Gemini),
                Some(json!([{"type":"web_search_20250305","name":"web_search","max_uses":5}])),
            )
            .await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{body}");
            assert!(!body.contains("upstream_error"), "{body}");
            let id = server_tool_fees::verify(&env, 21600, epoch).await;
            let row: (Value,Value)=sqlx::query_as("SELECT pricing_snapshot,usage_details FROM billing_records WHERE request_id::text=$1").bind(&id).fetch_one(&env.state.pg).await.unwrap();
            assert_eq!(row.0["server_tool_fees"][1]["requested"], false);
            assert!(row.0["server_tool_fees"][1]["quantity"].is_null());
            assert_eq!(
                row.0["server_tool_fees"][1]["pricing"]["price_per_request_micro"],
                500
            );
            assert_eq!(
                row.1["tokens"]["server_tool_usage"],
                json!({"provider":"anthropic","web_search_requests":2})
            );
        }
    }
}

#[tokio::test]
async fn requested_fetch_charge_requires_its_own_count_even_when_search_is_unrequested() {
    for stream in [false, true] {
        for count in [Some(3), None] {
            let mut usage = anthropic_usage::fixture();
            if let Some(count) = count {
                usage["server_tool_use"] = json!({"web_fetch_requests":count});
            }
            let env = setup(Protocol::Anthropic, usage).await;
            let profile = json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":{"billing":"additional","price_per_request_micro":10000},"web_fetch":{"billing":"additional","price_per_request_micro":500}});
            server_tool_fees::activate(&env, &profile).await;
            let response =
                request_with_tools(&env, Protocol::Anthropic, stream, false, Some(fetch())).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            let row = record(&env).await;
            let id = row["request_id"].as_str().unwrap();
            if count.is_some() {
                assert_eq!(status, 200, "{body}");
                assert!(!body.contains("upstream_error"), "{body}");
                assert_eq!(row["amount_micro"], 3100);
                let receipt: (i64,i64,i64,Option<i64>,Value)=sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot FROM billing_records WHERE request_id::text=$1")
                    .bind(id).fetch_one(&env.state.pg).await.unwrap();
                assert_eq!(
                    (receipt.0, receipt.1, receipt.2, receipt.3),
                    (3100, 3100, 0, Some(3875))
                );
                assert_eq!(receipt.4["server_tool_fees"][1]["amount_micro"], 1500);
                assert_eq!(receipt.4["server_tool_fees"][1]["quantity"], 3);
                assert!(receipt.4["server_tool_fees"][0]["quantity"].is_null());
                assert_eq!(receipt.4["server_tool_fees"][0]["requested"], false);
                assert_eq!(receipt.4["server_tool_fees"][1]["requested"], true);
                let expected = json!({"provider":"anthropic","web_fetch_requests":3});
                assert_eq!(row["usage"]["server_tool_usage"], expected);
                verify_published(&env, id, &expected, &receipt.4, [3100, 3100, 0, 3875]).await;
                let totals = report(&env, "/api/me/stats/breakdown?days=1").await;
                assert_eq!(totals["total"]["tokens"], 1050);
                assert_eq!(totals["total"]["amount_micro"], 3100);
            } else {
                assert_eq!(status, if stream { 200 } else { 502 }, "{body}");
                assert!(body.contains("upstream_error"), "{body}");
                assert_eq!(row["amount_micro"], 0);
            }
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000 - row["amount_micro"].as_i64().unwrap()
            );
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
        }
    }
}
