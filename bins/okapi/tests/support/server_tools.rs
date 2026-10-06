//! Actual gateway capture tests. Per-tool price integration has a separate acceptance stage.
use super::{Env, Protocol, anthropic_usage, record, report, request, setup};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

async fn verify_capture(env: &Env, expected: &Value) {
    let receipt = record(env).await;
    assert_eq!(
        receipt["usage"]["server_tool_usage"], *expected,
        "{receipt}"
    );
    assert_eq!(receipt["usage"]["prompt_tokens"], 1000);
    assert_eq!(receipt["usage"]["completion_tokens"], 50);
    assert_eq!(receipt["amount_micro"], 1600);
    let id = receipt["request_id"].as_str().unwrap();
    let stored: Value =
        sqlx::query_scalar("SELECT usage_details FROM billing_records WHERE request_id::text=$1")
            .bind(id)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(stored["tokens"]["server_tool_usage"], *expected);
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(payload["server_tool_usage"], *expected);
    for field in ["amount_micro", "original_amount_micro", "discount_micro"] {
        assert_eq!(payload[field], receipt[field]);
    }
    let ch = env.state.ch.as_ref().unwrap();
    ch.ensure_schema().await.unwrap();
    let mut rows = Vec::new();
    for _ in 0..100 {
        okapi::worker::chsink::process_once(&env.state.pg, ch)
            .await
            .unwrap();
        rows = ch.query_with_params("SELECT server_tool_usage FROM request_log_raw WHERE request_id=toUUID({id:String})", &[("id", id)]).await.unwrap();
        if !rows.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        rows.len(),
        1,
        "duplicate delivery must not add another tool observation"
    );
    let captured: Value =
        serde_json::from_str(rows[0]["server_tool_usage"].as_str().unwrap()).unwrap();
    assert_eq!(captured, *expected);
    verify_log_and_stats(env, id, expected).await;
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

async fn verify_log_and_stats(env: &Env, id: &str, expected: &Value) {
    let logs = report(env, &format!("/admin/logs?request_id={id}")).await;
    let rows = logs["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{logs}");
    assert_eq!(rows[0]["usage"]["server_tool_usage"], *expected);
    let stats = report(env, "/api/me/stats/breakdown?days=1").await;
    assert_eq!(stats["total"]["requests"], 1, "{stats}");
    assert_eq!(
        stats["total"]["tokens"], 1050,
        "tool counters must not enter Token statistics: {stats}"
    );
    assert_eq!(stats["total"]["amount_micro"], 1600, "{stats}");
}

#[tokio::test]
async fn server_tools_are_captured_once_across_all_ingresses_json_and_sse() {
    let mut final_usage = anthropic_usage::fixture();
    final_usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":3});
    let mock = json!({"final":final_usage,"start":{"input_tokens":100,"output_tokens":1,"server_tool_use":{"web_search_requests":0}},
        "updates":[{"server_tool_use":{"web_search_requests":1,"web_fetch_requests":2}},final_usage,final_usage,{"output_tokens":50},null]});
    let expected = json!({"provider":"anthropic","web_search_requests":2,"web_fetch_requests":3});
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, mock.clone()).await;
            // Prepare role before admission fills the auth cache, like other admin fixtures.
            sqlx::query("UPDATE users SET role=100 WHERE id=$1")
                .bind(env.user)
                .execute(&env.state.pg)
                .await
                .unwrap();
            let response =
                request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{ingress:?}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            verify_capture(&env, &expected).await;
        }
    }
}

#[tokio::test]
async fn server_tools_malformed_usage_refunds_and_later_updates_cannot_heal_it() {
    let valid = anthropic_usage::fixture();
    let mut invalid = valid.clone();
    invalid["server_tool_use"] = json!({"web_search_requests":-1});
    let mock = json!({"final":invalid,"start":{"input_tokens":100,"output_tokens":1},"updates":[invalid,valid]});
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, mock.clone()).await;
            let response =
                request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(
                status,
                if stream { 200 } else { 502 },
                "{ingress:?}: {body}"
            );
            assert!(body.contains("upstream_error"), "{body}");
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

#[tokio::test]
async fn server_tools_missing_zero_and_partial_counts_remain_distinct_in_real_receipts() {
    for (reported, expected) in [
        (Value::Null, Value::Null),
        (
            json!({"web_search_requests":0,"web_fetch_requests":0}),
            json!({"provider":"anthropic","web_search_requests":0,"web_fetch_requests":0}),
        ),
        (
            json!({"web_search_requests":0}),
            json!({"provider":"anthropic","web_search_requests":0}),
        ),
    ] {
        let mut usage = anthropic_usage::fixture();
        usage["server_tool_use"] = reported;
        for ingress in [
            Protocol::Anthropic,
            Protocol::Chat,
            Protocol::Responses,
            Protocol::Gemini,
        ] {
            for stream in [false, true] {
                let env = setup(Protocol::Anthropic, usage.clone()).await;
                let response =
                    request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
                let status = response.status();
                let body = response.text().await.unwrap();
                assert_eq!(status, 200, "{ingress:?}: {body}");
                let receipt = record(&env).await;
                assert_eq!(receipt["usage"]["server_tool_usage"], expected, "{receipt}");
                assert_eq!(receipt["amount_micro"], 1600);
                assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            }
        }
    }
}
