//! Real admin refunds preserve cost authority, including archived original bills.
use super::{
    Env, Protocol, anthropic_usage, record, report, request_with_tools, server_tool_fees, setup,
};
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn live_refunds_preserve_known_zero_and_unknown_cost_after_publication_change() {
    run(false).await;
}

#[tokio::test]
async fn archived_refunds_preserve_known_zero_and_unknown_cost_after_publication_change() {
    run(true).await;
}

async fn post(env: &Env, path: &str, body: &Value) -> Value {
    let response = reqwest::Client::new()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(&env.token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let value: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{path}: {value}");
    value
}

#[allow(clippy::too_many_lines)]
async fn run(archived: bool) {
    for cost in [Some(1250), Some(0), None] {
        for stream in [false, true] {
            let mut usage = anthropic_usage::fixture();
            usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":0});
            let env = setup(Protocol::Anthropic, usage).await;
            sqlx::query("UPDATE users SET role=100 WHERE id=$1")
                .bind(env.user)
                .execute(&env.state.pg)
                .await
                .unwrap();
            if let Some(cost) = cost {
                server_tool_fees::activate(
                    &env,
                    &server_tool_fees::prices(
                        &json!({"billing":"additional","price_per_request_micro":10000}),
                    ),
                )
                .await;
                sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
                    .bind(&env.model)
                    .bind(json!({"relative_cost_milli":cost}))
                    .execute(&env.state.pg)
                    .await
                    .unwrap();
            }
            let response = request_with_tools(
                &env,
                Protocol::Anthropic,
                stream,
                false,
                Some(json!([{"type":"web_search_20250305","name":"web_search","max_uses":5}])),
            )
            .await;
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let receipt = record(&env).await;
            let id = receipt["request_id"].as_str().unwrap();
            let original: (i64,i64,i64,Option<i64>,i64,Value,Value) = sqlx::query_as(
                "SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_epoch,pricing_snapshot,usage_details FROM billing_records WHERE request_id::text=$1")
                .bind(id).fetch_one(&env.state.pg).await.unwrap();
            let expected_amount = if cost.is_some() { 21600 } else { 1600 };
            let expected_cost = cost.map(|c| expected_amount * c / 1000);
            assert_eq!(
                (original.0, original.1, original.2, original.3),
                (expected_amount, expected_amount, 0, expected_cost)
            );
            if archived {
                sqlx::query("WITH removed AS (DELETE FROM billing_records WHERE request_id::text=$1 RETURNING *)
                    INSERT INTO billing_record_receipts(request_id,user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,status,
                    amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,node,pool,pricing_snapshot,usage_details,created_at,source_window)
                    SELECT request_id,user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,status,amount_micro,original_amount_micro,
                    discount_micro,upstream_cost_micro,is_stream,node,pool,pricing_snapshot,usage_details,created_at,source_window FROM removed")
                    .bind(id).execute(&env.state.pg).await.unwrap();
            }
            sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
                .bind(&env.model)
                .bind(json!({"relative_cost_milli":9000}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            let published = post(&env, "/admin/pricing/publish", &json!({})).await;
            assert!(published["epoch"].as_i64().unwrap() > original.4);
            env.state.pricebook.replace(
                okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
                    .await
                    .unwrap(),
            );
            env.state.channel_cost_cache.invalidate_all();
            let body = json!({"request_id":id,"reason":"refund provenance"});
            let first = post(&env, "/admin/billing/refund", &body).await;
            assert_eq!(first["refunded_micro"], expected_amount);
            assert_eq!(first["credited_micro"], expected_amount);
            assert_eq!(
                post(&env, "/admin/billing/refund", &body).await["outcome"],
                "already_refunded"
            );
            let events: Vec<Value> = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.refunded' AND payload->>'request_id'=$1")
                .bind(id).fetch_all(&env.state.pg).await.unwrap();
            assert_eq!(events.len(), 1);
            let event = &events[0];
            assert_eq!(event["upstream_cost_known"], expected_cost.is_some());
            assert_eq!(event["pricing_epoch"], original.4);
            assert_eq!(
                serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
                original.5
            );
            for (name, value) in [
                ("amount_micro", -expected_amount),
                ("original_amount_micro", -expected_amount),
                ("discount_micro", 0),
                ("upstream_cost_micro", -expected_cost.unwrap_or(0)),
            ] {
                assert_eq!(event[name], value);
            }
            for name in [
                "prompt_tokens",
                "cached_tokens",
                "completion_tokens",
                "reasoning_tokens",
            ] {
                assert_eq!(event[name], 0);
            }
            assert!(event["server_tool_usage"].is_null());
            verify_delivery(&env, id, event, &original.5).await;
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
            let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE user_id=$1")
                .bind(env.user)
                .fetch_one(&env.state.pg)
                .await
                .unwrap();
            assert_eq!(used, 0);
            let status: i16 = sqlx::query_scalar(
                "SELECT status FROM billing_financial_records WHERE request_id::text=$1",
            )
            .bind(id)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
            assert_eq!(status, 30);
            let totals = report(&env, "/api/me/stats/breakdown?days=1").await;
            assert_eq!(totals["total"]["amount_micro"], 0, "{totals}");
            assert_eq!(totals["total"]["tokens"], 1050, "{totals}");
            assert_eq!(totals["total"]["requests"], 1, "{totals}");
            let analytics = report(
                &env,
                &format!(
                    "/admin/stats/trend?days=1&granularity=hour&user_id={}",
                    env.user
                ),
            )
            .await;
            let total = &analytics["total"];
            assert_eq!(total["financial_records"], 2, "{analytics}");
            assert_eq!(
                total["cost_known_records"],
                if expected_cost.is_some() { 2 } else { 0 },
                "{analytics}"
            );
            assert_eq!(
                total["cost_coverage_bp"],
                if expected_cost.is_some() { 10000 } else { 0 },
                "{analytics}"
            );
            assert_eq!(total["known_cost_micro"], 0, "{analytics}");
            assert_eq!(total["known_amount_micro"], 0, "{analytics}");
            if expected_cost.is_some() {
                assert_eq!(total["margin_micro"], 0, "{analytics}");
            } else {
                assert!(total["margin_micro"].is_null(), "{analytics}");
            }
        }
    }
}

fn integer(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| value.as_str().unwrap().parse().unwrap())
}

async fn verify_delivery(env: &Env, id: &str, event: &Value, snapshot: &Value) {
    let ch = env.state.ch.as_ref().unwrap();
    ch.ensure_schema().await.unwrap();
    let mut rows = Vec::new();
    for _ in 0..100 {
        okapi::worker::chsink::process_once(&env.state.pg, ch)
            .await
            .unwrap();
        rows = ch.query_with_params("SELECT log_type,prompt_tokens,completion_tokens,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,upstream_cost_known,pricing_epoch,ratio_snapshot,server_tool_usage FROM request_log_raw WHERE request_id=toUUID({id:String}) ORDER BY log_type", &[("id",id)]).await.unwrap();
        if rows.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rows.len(), 2);
    assert_eq!(integer(&rows[0]["log_type"]), 2);
    assert_eq!(integer(&rows[1]["log_type"]), 6);
    let reverse = &rows[1];
    for name in [
        "amount_micro",
        "original_amount_micro",
        "discount_micro",
        "upstream_cost_micro",
    ] {
        assert_eq!(integer(&reverse[name]), integer(&event[name]));
        assert_eq!(integer(&rows[0][name]) + integer(&reverse[name]), 0);
    }
    assert_eq!(
        integer(&reverse["upstream_cost_known"]),
        i64::from(event["upstream_cost_known"].as_bool().unwrap())
    );
    assert_eq!(
        integer(&rows[0]["upstream_cost_known"]),
        integer(&reverse["upstream_cost_known"])
    );
    assert_eq!(
        integer(&reverse["pricing_epoch"]),
        integer(&event["pricing_epoch"])
    );
    assert_eq!(
        serde_json::from_str::<Value>(reverse["ratio_snapshot"].as_str().unwrap()).unwrap(),
        *snapshot
    );
    assert_eq!(reverse["server_tool_usage"], "");
    assert_eq!(integer(&reverse["prompt_tokens"]), 0);
    assert_eq!(integer(&reverse["completion_tokens"]), 0);
    assert_eq!(integer(&rows[0]["prompt_tokens"]), 1000);
    assert_eq!(integer(&rows[0]["completion_tokens"]), 50);
}
