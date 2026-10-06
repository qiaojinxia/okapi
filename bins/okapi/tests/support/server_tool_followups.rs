use super::{Protocol, anthropic_usage, record, request_with_tools, server_tool_fees, setup};
use serde_json::json;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn converted_response_model_prices_cannot_raise_or_remove_the_admitted_profile() {
    for ingress in [Protocol::Chat, Protocol::Responses, Protocol::Gemini] {
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
                server_tool_fees::activate(
                    &env,
                    &server_tool_fees::prices(
                        &json!({"billing":"additional","price_per_request_micro":10000}),
                    ),
                )
                .await;
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
                let response = request_with_tools(
                    &env,
                    ingress,
                    stream,
                    matches!(ingress, Protocol::Gemini),
                    Some(json!([{"type":"web_search_20250305","name":"web_search","max_uses":2}])),
                )
                .await;
                assert_eq!(response.status(), if stream { 200 } else { 502 });
                assert!(response.text().await.unwrap().contains("upstream_error"));
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
                assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            }
        }
    }
}

#[tokio::test]
async fn native_gemini_google_search_requires_channel_tool_capability() {
    let env = setup(Protocol::Gemini, Protocol::Gemini.fixture()).await;
    sqlx::query("UPDATE channels SET capabilities=$2 WHERE name=$1")
        .bind(&env.model)
        .bind(json!({"tools":false}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let response = request_with_tools(
        &env,
        Protocol::Gemini,
        false,
        true,
        Some(json!([{"googleSearch":{}}])),
    )
    .await;
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 503, "{body}");
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
}
