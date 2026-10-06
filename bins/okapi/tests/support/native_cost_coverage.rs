//! Native execution request counts do not prove complete container-duration cost.
use super::{
    Env, Protocol, anthropic_usage, record, report, request_with_tools, server_tool_fees, setup,
};
use serde_json::{Value, json};

fn usage() -> Value {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] =
        json!({"web_search_requests":2,"web_fetch_requests":0,"code_execution_requests":1});
    usage
}
fn tools() -> Value {
    json!([
        {"type":"web_search_20250305","name":"web_search","max_uses":5},
        {"type":"code_execution_20250825","name":"code_execution"}
    ])
}
async fn bill(env: &Env) -> (Option<i64>, Value) {
    let row = record(env).await;
    sqlx::query_as("SELECT upstream_cost_micro,pricing_snapshot FROM billing_records WHERE request_id::text=$1")
        .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap()
}
async fn activate(env: &Env) {
    server_tool_fees::activate(
        env,
        &server_tool_fees::prices(&json!({"billing":"additional","price_per_request_micro":10000})),
    )
    .await;
}

#[tokio::test]
async fn mixed_native_execution_does_not_claim_complete_search_only_cost() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            mixed_case(ingress, stream).await;
        }
    }
}
async fn mixed_case(ingress: Protocol, stream: bool) {
    let env = setup(Protocol::Anthropic, cumulative_usage()).await;
    activate(&env).await;
    let response = request_with_tools(
        &env,
        ingress,
        stream,
        matches!(ingress, Protocol::Gemini),
        Some(tools()),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert!(!response.text().await.unwrap().contains("upstream_error"));
    let row = record(&env).await;
    assert_eq!(row["amount_micro"], 21600);
    assert_eq!(row["usage"]["prompt_tokens"], 1000);
    assert_eq!(row["usage"]["completion_tokens"], 50);
    let (cost, snapshot) = bill(&env).await;
    assert_eq!(
        cost, None,
        "container-duration fees unavailable: {snapshot}"
    );
    assert_eq!(
        row["usage"]["server_tool_usage"]["code_execution_requests"],
        1
    );
    assert_eq!(snapshot["server_tool_cost_coverage"]["complete"], false);
    assert_eq!(snapshot["server_tool_cost_coverage"]["requested"], true);
    verify_receipts(&env, &row, &snapshot, false).await;
    let capture = env.captured.lock().await;
    let execution = capture[0].0["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "code_execution")
        .unwrap();
    assert_eq!(execution, &tools()[1]);
    assert!(execution["max_uses"].is_null());
}

#[tokio::test]
async fn observed_native_execution_count_survives_without_inventing_token_usage() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, cumulative_usage()).await;
            activate(&env).await;
            let response = request_with_tools(
                &env,
                ingress,
                stream,
                matches!(ingress, Protocol::Gemini),
                Some(json!([
                    {"type":"web_search_20250305","name":"web_search","max_uses":5}
                ])),
            )
            .await;
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 21600);
            assert_eq!(
                row["usage"]["server_tool_usage"]["code_execution_requests"],
                1
            );
            let (cost, snapshot) = bill(&env).await;
            assert_eq!(cost, None);
            assert_eq!(snapshot["server_tool_cost_coverage"]["requested"], false);
            verify_receipts(&env, &row, &snapshot, false).await;
        }
    }
}

fn cumulative_usage() -> Value {
    let final_usage = usage();
    json!({"final":final_usage,"start":{"input_tokens":100,"output_tokens":1,"server_tool_use":{"web_search_requests":0,"code_execution_requests":0}},
        "updates":[final_usage,final_usage,{"output_tokens":50}]})
}
fn integer(v: &Value) -> i64 {
    v.as_i64()
        .unwrap_or_else(|| v.as_str().unwrap().parse().unwrap())
}
async fn event(env: &Env, id: &str, topic: &str) -> Value {
    sqlx::query_scalar(
        "SELECT payload FROM billing_outbox WHERE topic=$2 AND payload->>'request_id'=$1",
    )
    .bind(id)
    .bind(topic)
    .fetch_one(&env.state.pg)
    .await
    .unwrap()
}
async fn delivered(env: &Env, id: &str, kind: i16) -> Value {
    let ch = env.state.ch.as_ref().unwrap();
    ch.ensure_schema().await.unwrap();
    for _ in 0..100 {
        okapi::worker::chsink::process_once(&env.state.pg, ch)
            .await
            .unwrap();
        let rows = ch.query_with_params("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,upstream_cost_known,ratio_snapshot,server_tool_usage,prompt_tokens,completion_tokens FROM request_log_raw WHERE request_id=toUUID({id:String}) AND log_type={kind:Int16}", &[("id",id),("kind",&kind.to_string())]).await.unwrap();
        if !rows.is_empty() {
            assert_eq!(rows.len(), 1);
            return rows[0].clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("missing native execution receipt")
}
async fn verify_receipts(env: &Env, row: &Value, snapshot: &Value, known: bool) {
    let id = row["request_id"].as_str().unwrap();
    let amount = row["amount_micro"].as_i64().unwrap();
    let stored:(i64,i64,i64,Option<i64>,Value) = sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,usage_details FROM billing_records WHERE request_id::text=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(
        (stored.0, stored.1, stored.2, stored.3),
        (amount, amount, 0, known.then_some(amount * 1250 / 1000))
    );
    assert_eq!(
        stored.4["tokens"]["server_tool_usage"],
        row["usage"]["server_tool_usage"]
    );
    let event = event(env, id, "billing.completed").await;
    assert_eq!(event["upstream_cost_known"], known);
    assert_eq!(
        event["server_tool_usage"],
        row["usage"]["server_tool_usage"]
    );
    let delivered = delivered(env, id, 2).await;
    for (name, expected) in [
        ("amount_micro", amount),
        ("original_amount_micro", amount),
        ("discount_micro", 0),
        ("upstream_cost_micro", stored.3.unwrap_or(0)),
    ] {
        assert_eq!(event[name], expected);
        assert_eq!(integer(&delivered[name]), expected);
    }
    assert_eq!(integer(&delivered["upstream_cost_known"]), i64::from(known));
    assert_eq!(
        serde_json::from_str::<Value>(delivered["server_tool_usage"].as_str().unwrap()).unwrap(),
        event["server_tool_usage"]
    );
    for value in [&event["ratio_snapshot"], &delivered["ratio_snapshot"]] {
        assert_eq!(
            serde_json::from_str::<Value>(value.as_str().unwrap()).unwrap(),
            *snapshot
        );
    }
    assert_eq!(env.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - amount
    );
    verify_statistics(env, amount, 1, i64::from(known), stored.3.unwrap_or(0)).await;
}
async fn verify_statistics(env: &Env, amount: i64, records: i64, known: i64, cost: i64) {
    let stats = report(env, "/api/me/stats/breakdown?days=1").await;
    assert_eq!(stats["total"]["tokens"], 1050);
    assert_eq!(stats["total"]["requests"], 1);
    assert_eq!(stats["total"]["amount_micro"], amount);
    let stats = report(
        env,
        &format!(
            "/admin/stats/trend?days=1&granularity=hour&user_id={}",
            env.user
        ),
    )
    .await;
    assert_eq!(stats["total"]["financial_records"], records, "{stats}");
    assert_eq!(stats["total"]["cost_known_records"], known, "{stats}");
    assert_eq!(
        stats["total"]["cost_coverage_bp"],
        if known == records { 10000 } else { 0 },
        "{stats}"
    );
    assert_eq!(stats["total"]["known_cost_micro"], cost, "{stats}");
}

#[tokio::test]
async fn declaration_missing_zero_and_ordinary_function_controls_keep_cost_coverage_distinct() {
    for (native, count) in [
        (true, Value::Null),
        (true, json!(0)),
        (false, Value::Null),
        (false, json!(0)),
    ] {
        for stream in [false, true] {
            let mut raw = usage();
            raw["server_tool_use"]["code_execution_requests"] = count.clone();
            let env = setup(Protocol::Anthropic, raw).await;
            activate(&env).await;
            let mut tools = tools();
            if !native {
                tools[1] = json!({"name":"code_execution","input_schema":{"type":"object","properties":{}}});
            }
            let response =
                request_with_tools(&env, Protocol::Anthropic, stream, false, Some(tools)).await;
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 21600);
            assert_eq!(
                row["usage"]["server_tool_usage"]["code_execution_requests"],
                count
            );
            let (cost, snapshot) = bill(&env).await;
            assert_eq!(cost.is_none(), native);
            assert_eq!(snapshot["server_tool_cost_coverage"].is_null(), !native);
            verify_receipts(&env, &row, &snapshot, !native).await;
        }
    }
}

#[tokio::test]
async fn execution_only_keeps_token_quote_without_fabricated_search_or_duration_fees() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let mut raw = anthropic_usage::fixture();
            raw["server_tool_use"] = json!({"code_execution_requests":1});
            let env = setup(Protocol::Anthropic, raw).await;
            activate(&env).await;
            let response = request_with_tools(
                &env,
                ingress,
                stream,
                matches!(ingress, Protocol::Gemini),
                Some(json!([tools()[1]])),
            )
            .await;
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 1600);
            let (cost, snapshot) = bill(&env).await;
            assert_eq!(cost, None);
            assert_eq!(
                snapshot["server_tool_admission"]["tools"],
                json!([tools()[1]])
            );
            let estimate = &snapshot["reservation"]["candidates"][0]["estimated_usage"];
            assert!(estimate["server_tool_usage"].is_null());
            assert!(row["usage"]["server_tool_usage"]["web_search_requests"].is_null());
            verify_receipts(&env, &row, &snapshot, false).await;
        }
    }
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
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body
}
#[tokio::test]
async fn native_unknown_cost_refunds_once_without_copying_execution_usage() {
    for stream in [false, true] {
        let env = setup(Protocol::Anthropic, cumulative_usage()).await;
        activate(&env).await;
        let response =
            request_with_tools(&env, Protocol::Anthropic, stream, false, Some(tools())).await;
        assert_eq!(response.status(), 200);
        assert!(!response.text().await.unwrap().contains("upstream_error"));
        let row = record(&env).await;
        let id = row["request_id"].as_str().unwrap();
        let (cost, snapshot) = bill(&env).await;
        assert_eq!(cost, None);
        verify_receipts(&env, &row, &snapshot, false).await;
        let body = json!({"request_id":id,"reason":"native cost coverage"});
        assert_eq!(
            post(&env, "/admin/billing/refund", &body).await["refunded_micro"],
            21600
        );
        assert_eq!(
            post(&env, "/admin/billing/refund", &body).await["outcome"],
            "already_refunded"
        );
        let reverse = event(&env, id, "billing.refunded").await;
        assert_eq!(reverse["upstream_cost_known"], false);
        assert_eq!(reverse["upstream_cost_micro"], 0);
        assert!(reverse["server_tool_usage"].is_null());
        assert_eq!(
            serde_json::from_str::<Value>(reverse["ratio_snapshot"].as_str().unwrap()).unwrap(),
            snapshot
        );
        let reverse = delivered(&env, id, 6).await;
        assert_eq!(integer(&reverse["amount_micro"]), -21600);
        assert_eq!(integer(&reverse["original_amount_micro"]), -21600);
        assert_eq!(integer(&reverse["discount_micro"]), 0);
        assert_eq!(integer(&reverse["upstream_cost_micro"]), 0);
        assert_eq!(integer(&reverse["upstream_cost_known"]), 0);
        assert_eq!(reverse["server_tool_usage"], "");
        assert_eq!(
            integer(&reverse["prompt_tokens"]) + integer(&reverse["completion_tokens"]),
            0
        );
        verify_statistics(&env, 0, 2, 0, 0).await;
        assert_eq!(
            env.state
                .ledger
                .balance(env.user)
                .await
                .unwrap()
                .as_micros(),
            50_000_000
        );
        assert_eq!(bill(&env).await, (None, snapshot));
    }
}

#[tokio::test]
async fn durable_execution_usage_and_unknown_cost_replay_frozen_metadata_once() {
    use fred::interfaces::HashesInterface;
    for stream in [false, true] {
        let env = setup(Protocol::Anthropic, cumulative_usage()).await;
        activate(&env).await;
        let rule = format!("native_no_success_{}", env.user);
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE billing_records ADD CONSTRAINT {rule} CHECK(user_id<>{} OR status<>20)",
            env.user
        )))
        .execute(&env.state.pg)
        .await
        .unwrap();
        let response =
            request_with_tools(&env, Protocol::Anthropic, stream, false, Some(tools())).await;
        let status = response.status();
        let body = response.text().await.unwrap();
        let held = env.state.ledger.list_reservations(env.user).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE billing_records DROP CONSTRAINT {rule}"
        )))
        .execute(&env.state.pg)
        .await
        .unwrap();
        assert_eq!(status, 200, "{body}");
        assert!(!body.contains("upstream_error"));
        assert_eq!(held.len(), 1);
        let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
            .await
            .unwrap();
        let payload: String = redis
            .hget(
                "settlement:{retry}:payloads",
                held[0].request_id.to_string(),
            )
            .await
            .unwrap();
        let mut saved: okapi_ledger::pg::OwnedSettlementInput =
            serde_json::from_str(&payload).unwrap();
        assert_eq!(saved.amount.as_micros(), 21600);
        assert_eq!(saved.upstream_cost, None);
        assert_eq!(
            saved.pricing_snapshot.as_ref().unwrap()["server_tool_cost_coverage"]["observed_requests"],
            1
        );
        assert_eq!(
            saved.usage.server_tool_usage.unwrap(),
            okapi_domain::ServerToolUsage::Anthropic(okapi_domain::AnthropicToolUsage {
                web_search_requests: Some(2),
                web_fetch_requests: Some(0),
                code_execution_requests: Some(1)
            })
        );
        sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
            .bind(&env.model)
            .bind(json!({"relative_cost_milli":9000}))
            .execute(&env.state.pg)
            .await
            .unwrap();
        env.state.channel_cost_cache.invalidate_all();
        // An already filled Token/search-only estimate cannot bypass incomplete coverage.
        saved.upstream_cost = Some(okapi_domain::Money::from_micros(27000));
        assert!(env.state.settle_success(saved.as_input()).await.unwrap());
        assert!(!env.state.settle_success(saved.as_input()).await.unwrap());
        let row = record(&env).await;
        let (cost, snapshot) = bill(&env).await;
        assert_eq!(cost, None);
        assert_eq!(Some(&snapshot), saved.pricing_snapshot.as_ref());
        verify_receipts(&env, &row, &snapshot, false).await;
        assert!(
            env.state
                .ledger
                .list_reservations(env.user)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn malformed_execution_counts_fail_closed_and_release_the_actual_hold() {
    for count in [json!(-1), json!("1"), json!(2_147_483_648_u64), json!([])] {
        for stream in [false, true] {
            let mut raw = usage();
            raw["server_tool_use"]["code_execution_requests"] = count.clone();
            let env = setup(Protocol::Anthropic, raw).await;
            activate(&env).await;
            let response =
                request_with_tools(&env, Protocol::Anthropic, stream, false, Some(tools())).await;
            assert_eq!(response.status(), if stream { 200 } else { 502 });
            assert!(response.text().await.unwrap().contains("upstream_error"));
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 0);
            assert_eq!(bill(&env).await.0, None);
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
            assert!(
                env.state
                    .ledger
                    .list_reservations(env.user)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
