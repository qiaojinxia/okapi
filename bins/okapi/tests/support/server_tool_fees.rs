//! Actual price publication and monetary receipts, independently of capture-only tests.
use super::{Env, Protocol, anthropic_usage, record, report, request_with_tools, setup};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

async fn request(
    env: &Env,
    protocol: Protocol,
    stream: bool,
    native_gemini: bool,
) -> reqwest::Response {
    request_with_tools(
        env,
        protocol,
        stream,
        native_gemini,
        Some(json!([
            {"type":"web_search_20250305","name":"web_search","max_uses":5},
            {"type":"web_fetch_20250910","name":"web_fetch","max_uses":5}
        ])),
    )
    .await
}

pub(super) fn prices(search: &Value) -> Value {
    json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":search,"web_fetch":{"billing":"included"}})
}
fn integer(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| value.as_str().unwrap().parse().unwrap())
}
async fn post(env: &Env, path: &str, body: &Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(&env.token)
        .json(body)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
async fn write_draft(env: &Env, profile: &Value) {
    let (status,body)=post(env,"/admin/models", &json!({"model_name":env.model,"model_ratio":"1",
        "completion_ratio":"2","cache_ratio":"0.5","cache_write_ratio":"2",
        "audio_ratio":"8","audio_completion_ratio":"2","image_ratio":"3","server_tool_prices":profile})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["requires_publish"], true);
}
pub(super) async fn activate(env: &Env, profile: &Value) -> i64 {
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(env.user)
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
        .bind(env.model.clone())
        .bind(json!({"relative_cost_milli":1250}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let old_epoch = env.state.pricebook.epoch();
    write_draft(env, profile).await;
    let catalog = report(env, &format!("/api/pricing?model={}", env.model)).await;
    assert!(
        catalog["models"][0]["server_tool_prices"].is_null(),
        "draft leaked: {catalog}"
    );
    let old = okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(old.epoch(), old_epoch);
    let (status, body) = post(env, "/admin/pricing/publish", &json!({})).await;
    assert_eq!(status, 200, "{body}");
    let epoch = body["epoch"].as_i64().unwrap();
    assert!(epoch > old_epoch);
    let book = okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(book.epoch(), epoch);
    assert!(env.state.pricebook.swap_if_newer(book));
    let catalog = report(env, &format!("/api/pricing?model={}", env.model)).await;
    assert_eq!(
        catalog["models"][0]["server_tool_prices"], *profile,
        "{catalog}"
    );
    epoch
}
async fn ch_row(env: &Env, id: &str, kind: i16) -> Value {
    let ch = env.state.ch.as_ref().unwrap();
    ch.ensure_schema().await.unwrap();
    for _ in 0..100 {
        okapi::worker::chsink::process_once(&env.state.pg, ch)
            .await
            .unwrap();
        let rows=ch.query_with_params("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,ratio_snapshot FROM request_log_raw WHERE request_id=toUUID({id:String}) AND log_type={kind:Int16}",&[("id",id),("kind",&kind.to_string())]).await.unwrap();
        if !rows.is_empty() {
            assert_eq!(rows.len(), 1);
            return rows[0].clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("missing monetary ClickHouse receipt")
}
pub(super) async fn verify(env: &Env, amount: i64, epoch: i64) -> String {
    let receipt = record(env).await;
    assert_eq!(receipt["amount_micro"], amount, "{receipt}");
    let id = receipt["request_id"].as_str().unwrap().to_owned();
    let row: (i64,i64,i64,Option<i64>,i64,Value)=sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_epoch,pricing_snapshot FROM billing_records WHERE request_id::text=$1")
        .bind(&id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(
        (row.0, row.1, row.2, row.3, row.4),
        (amount, amount, 0, Some(amount * 1250 / 1000), epoch)
    );
    assert_eq!(row.5["server_tool_fees"][0]["quantity"], 2);
    assert_eq!(row.5["server_tool_fees"][0]["amount_micro"], amount - 1600);
    assert_eq!(row.5["server_tool_fees"][1]["amount_micro"], 0);
    let event: Value=sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(&id).fetch_one(&env.state.pg).await.unwrap();
    for (name, expected) in [
        ("amount_micro", row.0),
        ("original_amount_micro", row.1),
        ("discount_micro", row.2),
        ("upstream_cost_micro", row.3.unwrap()),
    ] {
        assert_eq!(event[name], expected, "{event}");
    }
    assert_eq!(
        serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
        row.5
    );
    let ch = ch_row(env, &id, 2).await;
    for name in [
        "amount_micro",
        "original_amount_micro",
        "discount_micro",
        "upstream_cost_micro",
    ] {
        assert_eq!(integer(&ch[name]), integer(&event[name]), "{ch}");
    }
    assert_eq!(
        serde_json::from_str::<Value>(ch["ratio_snapshot"].as_str().unwrap()).unwrap(),
        row.5
    );
    let stats = report(env, "/api/me/stats/breakdown?days=1").await;
    assert_eq!(stats["total"]["tokens"], 1050, "{stats}");
    assert_eq!(stats["total"]["amount_micro"], amount, "{stats}");
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - amount
    );
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
    id
}

#[tokio::test]
async fn published_native_tool_fees_reach_all_ingresses_and_refund_once() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":3});
    let mock = json!({"final":usage,"start":{"input_tokens":100,"output_tokens":1,"server_tool_use":{"web_search_requests":0}},"updates":[usage,usage,{"output_tokens":50}]});
    let profile = prices(&json!({"billing":"additional","price_per_request_micro":10000}));
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, mock.clone()).await;
            let epoch = activate(&env, &profile).await;
            // Draft mutation after publication must not change reload or the public catalog.
            write_draft(
                &env,
                &prices(&json!({"billing":"additional","price_per_request_micro":50000})),
            )
            .await;
            env.state.pricebook.replace(
                okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
                    .await
                    .unwrap(),
            );
            let response =
                request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let id = verify(&env, 21600, epoch).await;
            let body = json!({"request_id":id,"reason":"independent tool fee acceptance"});
            let (status, result) = post(&env, "/admin/billing/refund", &body).await;
            assert_eq!(status, 200, "{result}");
            assert_eq!(result["refunded_micro"], 21600);
            let (status, result) = post(&env, "/admin/billing/refund", &body).await;
            assert_eq!(status, 200);
            assert_eq!(result["outcome"], "already_refunded");
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
            let reverse = ch_row(&env, &id, 6).await;
            assert_eq!(integer(&reverse["amount_micro"]), -21600);
            assert_eq!(integer(&reverse["original_amount_micro"]), -21600);
            assert_eq!(integer(&reverse["discount_micro"]), 0);
            assert_eq!(integer(&reverse["upstream_cost_micro"]), -27000);
            let totals = report(&env, "/api/me/stats/breakdown?days=1").await;
            assert_eq!(totals["total"]["amount_micro"], 0, "{totals}");
            assert_eq!(
                totals["total"]["tokens"], 1050,
                "refund must not invent new tool/Token usage: {totals}"
            );
        }
    }
}

#[tokio::test]
async fn explicit_free_and_included_tool_fees_do_not_double_charge() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":3});
    for price in [
        json!({"billing":"included"}),
        json!({"billing":"additional","price_per_request_micro":0}),
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, usage.clone()).await;
            let epoch = activate(&env, &prices(&price)).await;
            let response = request(&env, Protocol::Anthropic, stream, false).await;
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            verify(&env, 1600, epoch).await;
        }
    }
}

#[tokio::test]
async fn missing_published_paid_counter_refunds_instead_of_assuming_zero() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_fetch_requests":2});
    for stream in [false, true] {
        let env = setup(Protocol::Anthropic, usage.clone()).await;
        activate(
            &env,
            &prices(&json!({"billing":"additional","price_per_request_micro":10000})),
        )
        .await;
        let response = request(&env, Protocol::Anthropic, stream, false).await;
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

#[tokio::test]
async fn unconfigured_tool_cost_is_unknown_and_invalid_drafts_cannot_replace_prices() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":3});
    let env = setup(Protocol::Anthropic, usage).await;
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(env.user)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let (status, body) = post(
        &env,
        "/admin/models",
        &json!({"model_name":env.model,"model_ratio":"1",
        "server_tool_prices":prices(&json!({"billing":"additional","price_per_request_micro":-1}))}),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let stored: Option<Value> = sqlx::query_scalar("SELECT p.server_tool_prices FROM model_pricing p JOIN models m ON m.id=p.model_id WHERE m.model_name=$1")
        .bind(&env.model).fetch_one(&env.state.pg).await.unwrap();
    assert!(stored.is_none());
    let response = request(&env, Protocol::Anthropic, false, false).await;
    assert_eq!(response.status(), 200);
    response.text().await.unwrap();
    let receipt = record(&env).await;
    assert_eq!(receipt["amount_micro"], 1600);
    let id = receipt["request_id"].as_str().unwrap();
    let stored: (Option<i64>,Value) = sqlx::query_as("SELECT upstream_cost_micro,pricing_snapshot FROM billing_records WHERE request_id::text=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert!(stored.0.is_none());
    assert!(stored.1["server_tool_fees"][0]["amount_micro"].is_null());
    let event: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(id).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(event["upstream_cost_known"], false);
    assert_eq!(event["upstream_cost_micro"], 0);
    let ch = ch_row(&env, id, 2).await;
    assert_eq!(integer(&ch["upstream_cost_micro"]), 0);
    assert_eq!(
        serde_json::from_str::<Value>(ch["ratio_snapshot"].as_str().unwrap()).unwrap(),
        stored.1
    );
}

#[tokio::test]
async fn native_search_error_blocks_are_not_extra_billable_actions() {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":0,"web_fetch_requests":0});
    let mock = json!({"final":usage,"content":[
        {"type":"server_tool_use","id":"srv_search","name":"web_search","input":{"query":"fixture"}},
        {"type":"web_search_tool_result","tool_use_id":"srv_search","content":{"type":"web_search_tool_result_error","error_code":"too_many_requests"}}
    ]});
    for stream in [false, true] {
        let env = setup(Protocol::Anthropic, mock.clone()).await;
        let epoch = activate(
            &env,
            &prices(&json!({"billing":"additional","price_per_request_micro":10000})),
        )
        .await;
        let response = request(&env, Protocol::Anthropic, stream, false).await;
        assert_eq!(response.status(), 200);
        let body = response.text().await.unwrap();
        assert!(body.contains("web_search_tool_result_error"), "{body}");
        let receipt = record(&env).await;
        assert_eq!(receipt["amount_micro"], 1600);
        let id = receipt["request_id"].as_str().unwrap();
        let snapshot: Value = sqlx::query_scalar(
            "SELECT pricing_snapshot FROM billing_records WHERE request_id::text=$1",
        )
        .bind(id)
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
        assert_eq!(snapshot["epoch"], epoch);
        assert_eq!(snapshot["server_tool_fees"][0]["quantity"], 0);
        assert_eq!(snapshot["server_tool_fees"][0]["amount_micro"], 0);
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
}
