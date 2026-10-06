//! Admission assertions observe the real Redis hold from inside the HTTP upstream.
use super::{Env, Protocol, anthropic_usage, record, request_with_tools, server_tool_fees, setup};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

fn native(cap: Option<u32>) -> Value {
    let mut t =
        json!({"type":"web_search_20250305","name":"web_search","allowed_domains":["example.org"]});
    if let Some(cap) = cap {
        t["max_uses"] = json!(cap);
    }
    json!([t])
}
async fn publish(env: &Env, unit: i64) -> i64 {
    server_tool_fees::activate(
        env,
        &server_tool_fees::prices(&json!({"billing":"additional","price_per_request_micro":unit})),
    )
    .await
}
async fn call(env: &Env, tools: Value, stream: bool) -> reqwest::Response {
    request_with_tools(env, Protocol::Anthropic, stream, false, Some(tools)).await
}
async fn assert_refunded(env: &Env) {
    let row = record(env).await;
    assert_eq!(row["amount_micro"], 0, "{row}");
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000
    );
}

#[tokio::test]
async fn native_default_cap_is_forwarded_and_actual_hold_includes_tool_fees() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":0});
    for stream in [false, true] {
        let env = setup(Protocol::Anthropic, usage.clone()).await;
        let epoch = publish(&env, 10000).await;
        let response = call(&env, native(None), stream).await;
        assert_eq!(response.status(), 200);
        assert!(!response.text().await.unwrap().contains("upstream_error"));
        let id = server_tool_fees::verify(&env, 21600, epoch).await;
        let capture = env.captured.lock().await;
        assert_eq!(capture.len(), 1);
        assert_eq!(capture[0].0["tools"][0]["max_uses"], 5);
        assert_eq!(
            capture[0].0["tools"][0]["allowed_domains"],
            json!(["example.org"])
        );
        let reserved = 50_000_000 - capture[0].1;
        assert!(reserved >= 50000, "Token-only reservation: {reserved}");
        let snapshot: Value = sqlx::query_scalar(
            "SELECT pricing_snapshot FROM billing_records WHERE request_id::text=$1",
        )
        .bind(id)
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
        assert_eq!(
            snapshot["server_tool_admission"]["reserved_amount_micro"],
            reserved
        );
        assert_eq!(
            snapshot["server_tool_admission"]["tools"],
            capture[0].0["tools"]
        );
    }
}

#[tokio::test]
async fn priced_native_tools_cannot_cross_the_upstream_with_only_token_funds() {
    let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
    publish(&env, 60_000_000).await;
    let response = call(&env, native(Some(1)), false).await;
    assert_eq!(response.status(), 429);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("insufficient_quota")
    );
    assert_eq!(env.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000
    );
    let records: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1")
        .bind(env.user)
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(records, 0);
}

#[tokio::test]
async fn absent_whole_usage_over_cap_and_function_name_are_not_authorized_paid_usage() {
    for stream in [false, true] {
        for (counts, tools) in [
            (None, native(Some(2))),
            (
                Some(json!({"web_search_requests":3,"web_fetch_requests":0})),
                native(Some(2)),
            ),
            (
                Some(json!({"web_search_requests":1,"web_fetch_requests":0})),
                json!([{"name":"web_search","input_schema":{"type":"object"}}]),
            ),
        ] {
            let mut usage = anthropic_usage::fixture();
            if let Some(counts) = counts {
                usage["server_tool_use"] = counts;
            }
            let env = setup(Protocol::Anthropic, usage).await;
            publish(&env, 10000).await;
            let response = call(&env, tools, stream).await;
            assert_eq!(response.status(), if stream { 200 } else { 502 });
            assert!(response.text().await.unwrap().contains("upstream_error"));
            assert_refunded(&env).await;
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn channel_native_tool_mutation_refunds_before_any_upstream_call() {
    for setting in [
        json!({"strip_request_fields":["tools"]}),
        json!({"inject_request_fields":{"tools":[{"type":"web_search_20250305","name":"web_search"}]}}),
        json!({"inject_request_fields":{"tools":[{"type":"web_search_20250305","name":"web_search","max_uses":9}]}}),
        json!({"inject_request_fields":{"tools":[{"type":"web_fetch_20250910","name":"web_fetch","max_uses":2}]}}),
    ] {
        let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
        publish(&env, 10000).await;
        sqlx::query("UPDATE channels SET settings=$2 WHERE name=$1")
            .bind(&env.model)
            .bind(setting)
            .execute(&env.state.pg)
            .await
            .unwrap();
        let response = call(&env, native(Some(2)), false).await;
        assert_eq!(response.status(), 502);
        assert!(response.text().await.unwrap().contains("upstream_error"));
        assert_refunded(&env).await;
        assert_eq!(env.calls.load(Ordering::SeqCst), 0);
    }
}

async fn primary(env: &Env) -> String {
    let primary = format!("{}-primary", env.model);
    let response = reqwest::Client::new().post(format!("http://{}/admin/models",env.console)).bearer_auth(&env.token)
        .json(&json!({"model_name":primary,"model_ratio":"1","completion_ratio":"2", "fallback_models":[env.model],
            "server_tool_prices":server_tool_fees::prices(&json!({"billing":"additional","price_per_request_micro":1000}))}))
        .send().await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    let response = reqwest::Client::new()
        .post(format!("http://{}/admin/pricing/publish", env.console))
        .bearer_auth(&env.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.text().await.unwrap();
    env.state.pricebook.replace(
        okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
            .await
            .unwrap(),
    );
    primary
}
async fn call_primary(env: &Env, primary: &str) -> reqwest::Response {
    reqwest::Client::new().post(format!("http://{}/v1/messages",env.gateway)).bearer_auth(&env.token)
        .json(&json!({"model":primary,"messages":[{"role":"user","content":"hi"}],"max_tokens":512,"tools":native(None)}))
        .send().await.unwrap()
}

#[tokio::test]
async fn dearer_fallback_is_reserved_before_admission_and_billed_from_frozen_context() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":0});
    let env = setup(Protocol::Anthropic, usage).await;
    publish(&env, 10000).await;
    let primary = primary(&env).await;
    let response = call_primary(&env, &primary).await;
    assert_eq!(response.status(), 200);
    assert!(!response.text().await.unwrap().contains("upstream_error"));
    let row = record(&env).await;
    assert_eq!(row["amount_micro"], 21600, "{row}");
    let id = row["request_id"].as_str().unwrap();
    let snapshot: Value = sqlx::query_scalar(
        "SELECT pricing_snapshot FROM billing_records WHERE request_id::text=$1",
    )
    .bind(id)
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    assert_eq!(snapshot["requested_model"], primary);
    let capture = env.captured.lock().await;
    let reserved = 50_000_000 - capture[0].1;
    assert!(reserved >= 50000, "fallback fees not held: {reserved}");
    assert_eq!(
        snapshot["server_tool_admission"]["reserved_amount_micro"],
        reserved
    );

    let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
    publish(&env, 60_000_000).await;
    let primary = self::primary(&env).await;
    let response = call_primary(&env, &primary).await;
    assert_eq!(response.status(), 429);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("insufficient_quota")
    );
    assert_eq!(env.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000
    );
}

#[tokio::test]
async fn declared_unpriced_tools_with_no_observation_keep_cost_unknown() {
    let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
    sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
        .bind(&env.model)
        .bind(json!({"relative_cost_milli":1250}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let response = call(&env, native(Some(2)), false).await;
    assert_eq!(response.status(), 200);
    response.text().await.unwrap();
    let row = record(&env).await;
    assert_eq!(row["amount_micro"], 1600);
    let id = row["request_id"].as_str().unwrap();
    let stored: (Option<i64>, Value) = sqlx::query_as("SELECT upstream_cost_micro,pricing_snapshot FROM billing_records WHERE request_id::text=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert!(stored.0.is_none());
    assert!(stored.1["server_tool_fees"][0]["quantity"].is_null());
    assert!(stored.1["server_tool_fees"][0]["pricing"].is_null());
    let event: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(event["upstream_cost_known"], false);
    assert!(event["server_tool_usage"].is_null());
}

#[tokio::test]
async fn response_model_tool_prices_cannot_raise_or_remove_the_admitted_profile() {
    for stream in [false, true] {
        for price in [Some(60_000_000), None] {
            let response_model = format!("native-response-{}", uuid::Uuid::new_v4().simple());
            let mut usage = anthropic_usage::fixture();
            usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":0});
            let env = setup(
                Protocol::Anthropic,
                json!({"final":usage,"response_model":response_model}),
            )
            .await;
            publish(&env, 10000).await;
            let mut draft =
                json!({"model_name":response_model,"model_ratio":"1","completion_ratio":"2"});
            if let Some(price) = price {
                draft["server_tool_prices"] = server_tool_fees::prices(
                    &json!({"billing":"additional","price_per_request_micro":price}),
                );
            }
            let response = reqwest::Client::new()
                .post(format!("http://{}/admin/models", env.console))
                .bearer_auth(&env.token)
                .json(&draft)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.text().await.unwrap();
            let response = reqwest::Client::new()
                .post(format!("http://{}/admin/pricing/publish", env.console))
                .bearer_auth(&env.token)
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.text().await.unwrap();
            env.state.pricebook.replace(
                okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
                    .await
                    .unwrap(),
            );
            sqlx::query("UPDATE channels SET settings=$2 WHERE name=$1")
                .bind(&env.model)
                .bind(json!({"bill_by_response_model":true}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            let response = call(&env, native(Some(2)), stream).await;
            assert_eq!(response.status(), if stream { 200 } else { 502 });
            assert!(response.text().await.unwrap().contains("upstream_error"));
            assert_refunded(&env).await;
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn other_provider_builtin_tools_still_reach_the_native_responses_endpoint() {
    for stream in [false, true] {
        for tool in [
            json!({"type":"web_search_preview"}),
            json!({"type":"web_search_preview_2025_03_11"}),
        ] {
            let env = setup(Protocol::Responses, Protocol::Responses.fixture()).await;
            let tools = json!([tool]);
            let response = request_with_tools(
                &env,
                Protocol::Responses,
                stream,
                false,
                Some(tools.clone()),
            )
            .await;
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let capture = env.captured.lock().await;
            assert_eq!(capture.len(), 1);
            assert_eq!(capture[0].0["tools"], tools);
        }
    }
}
