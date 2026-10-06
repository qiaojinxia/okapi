//! Cost must follow the selected channel snapshot, not a later config read.
use super::{
    CostGate, Env, Protocol, anthropic_usage, record, request_with_tools, server_tool_fees,
    server_tool_scope, setup_with_pricing_and_cost_gate,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

fn search() -> Value {
    json!([{"type":"web_search_20250305","name":"web_search","max_uses":5}])
}

#[tokio::test]
async fn changing_channel_cost_after_upstream_admission_does_not_reprice_the_request() {
    run(1250, 27000).await;
}

#[tokio::test]
async fn explicit_zero_channel_cost_remains_known_zero_after_config_change() {
    run(0, 0).await;
}

async fn run(initial_cost: i64, expected_cost: i64) {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":0});
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let gate = Arc::new(CostGate::default());
            let env = Arc::new(
                setup_with_pricing_and_cost_gate(
                    Protocol::Anthropic,
                    usage.clone(),
                    None,
                    Some(gate.clone()),
                )
                .await,
            );
            server_tool_fees::activate(
                &env,
                &server_tool_fees::prices(
                    &json!({"billing":"additional","price_per_request_micro":10000}),
                ),
            )
            .await;
            sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
                .bind(&env.model)
                .bind(json!({"relative_cost_milli":initial_cost}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            let call = {
                let env = env.clone();
                tokio::spawn(async move {
                    let response = request_with_tools(
                        &env,
                        ingress,
                        stream,
                        matches!(ingress, Protocol::Gemini),
                        Some(search()),
                    )
                    .await;
                    let status = response.status();
                    let body = response.text().await.unwrap();
                    (status, body)
                })
            };
            tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
                .await
                .unwrap();
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
                .bind(&env.model)
                .bind(json!({"relative_cost_milli":2500}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            env.state.channel_cost_cache.invalidate_all();
            gate.release.notify_one();
            let (status, body) = tokio::time::timeout(Duration::from_secs(10), call)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status, 200, "{ingress:?}/{stream}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            verify(&env, ingress, stream, initial_cost, expected_cost).await;
        }
    }
}

async fn verify(env: &Env, ingress: Protocol, stream: bool, initial_cost: i64, expected_cost: i64) {
    let row = record(env).await;
    let id = row["request_id"].as_str().unwrap();
    let receipt:(i64,i64,i64,Option<i64>,Value,i64)=sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,channel_id FROM billing_records WHERE request_id::text=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(
        (receipt.0, receipt.1, receipt.2, receipt.3),
        (21600, 21600, 0, Some(expected_cost)),
        "{ingress:?}/{stream}: {receipt:?}"
    );
    assert_eq!(
        receipt.4["upstream_cost_basis"],
        json!({"version":1,"source":"selected_channel","channel_id":receipt.5,"relative_cost_milli":initial_cost,"list_price_micro":21600})
    );
    let expected = json!({"provider":"anthropic","web_search_requests":2,"web_fetch_requests":0});
    server_tool_scope::verify_published(
        env,
        id,
        &expected,
        &receipt.4,
        [21600, 21600, 0, expected_cost],
    )
    .await;
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - 21600
    );
}
