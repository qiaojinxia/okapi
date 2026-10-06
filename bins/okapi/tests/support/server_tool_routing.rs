//! Native tool authority participates in capability routing for every ingress.
use super::{
    Protocol, anthropic_usage, record, report, request_with_tools, server_tool_fees,
    server_tool_scope, setup,
};
use serde_json::json;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn native_tools_cannot_bypass_explicit_channel_denial_and_plain_requests_still_route() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
            sqlx::query("UPDATE channels SET capabilities=$2 WHERE name=$1")
                .bind(&env.model)
                .bind(json!({"tools":false}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            let response = request_with_tools(
                &env,
                ingress,
                stream,
                matches!(ingress, Protocol::Gemini),
                Some(json!([{"type":"web_fetch_20250910","name":"web_fetch","max_uses":5}])),
            )
            .await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 503, "{ingress:?}/{stream}: {body}");
            assert!(body.contains("no_available_channel"), "{body}");
            assert_eq!(env.calls.load(Ordering::SeqCst), 0);
            assert_eq!(record(&env).await["amount_micro"], 0);
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
            let control = request_with_tools(
                &env,
                ingress,
                stream,
                matches!(ingress, Protocol::Gemini),
                None,
            )
            .await;
            let control_status = control.status();
            let control_body = control.text().await.unwrap();
            assert_eq!(control_status, 200, "{ingress:?}/{stream}: {control_body}");
            assert!(!control_body.contains("upstream_error"), "{control_body}");
            assert_eq!(record(&env).await["amount_micro"], 1600);
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000 - 1600
            );
        }
    }
}

#[tokio::test]
async fn no_declared_tool_and_partial_observed_zero_do_not_require_unused_paid_counts() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let mut usage = anthropic_usage::fixture();
            usage["server_tool_use"] = json!({"web_search_requests":0});
            let env = setup(Protocol::Anthropic, usage).await;
            server_tool_fees::activate(&env, &json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":{"billing":"additional","price_per_request_micro":10000},"web_fetch":{"billing":"additional","price_per_request_micro":500}})).await;
            let response = request_with_tools(
                &env,
                ingress,
                stream,
                matches!(ingress, Protocol::Gemini),
                None,
            )
            .await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{ingress:?}/{stream}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 1600);
            let expected = json!({"provider":"anthropic","web_search_requests":0});
            assert_eq!(row["usage"]["server_tool_usage"], expected);
            let receipt: (i64, i64, i64, Option<i64>, serde_json::Value, serde_json::Value) = sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,usage_details FROM billing_records WHERE request_id::text=$1")
                .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
            assert_eq!(
                (receipt.0, receipt.1, receipt.2, receipt.3),
                (1600, 1600, 0, Some(2000))
            );
            assert_eq!(receipt.5["tokens"]["server_tool_usage"], expected);
            for fee in receipt.4["server_tool_fees"].as_array().unwrap() {
                assert_eq!(fee["requested"], false);
                assert_eq!(fee["amount_micro"], 0);
            }
            assert_eq!(receipt.4["server_tool_fees"][0]["quantity"], 0);
            assert!(receipt.4["server_tool_fees"][1]["quantity"].is_null());
            server_tool_scope::verify_published(
                &env,
                row["request_id"].as_str().unwrap(),
                &expected,
                &receipt.4,
                [1600, 1600, 0, 2000],
            )
            .await;
            let totals = report(&env, "/api/me/stats/breakdown?days=1").await;
            assert_eq!(totals["total"]["tokens"], 1050);
            assert_eq!(totals["total"]["amount_micro"], 1600);
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000 - 1600
            );
        }
    }
}
