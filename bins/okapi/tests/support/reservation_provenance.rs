//! Admission estimates remain distinct from observed usage, actual fees and replay.
use super::{
    CostGate, Env, Protocol, anthropic_usage, record, report, request_with_tools, server_tool_fees,
    server_tool_scope, setup, setup_with_pricing_and_cost_gate,
};
use fred::interfaces::{HashesInterface, KeysInterface};
use serde_json::{Value, json};
use sha2::Digest;
use std::{sync::Arc, time::Duration};

async fn redis() -> fred::clients::Client {
    okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap()
}

fn search() -> Value {
    json!([{"type":"web_search_20250305","name":"web_search","max_uses":5}])
}
fn usage() -> Value {
    let mut usage = anthropic_usage::fixture();
    usage["server_tool_use"] = json!({"web_search_requests":2,"web_fetch_requests":0});
    usage
}
async fn snapshot(env: &Env) -> Value {
    let row = record(env).await;
    sqlx::query_scalar("SELECT pricing_snapshot FROM billing_records WHERE request_id::text=$1")
        .bind(row["request_id"].as_str().unwrap())
        .fetch_one(&env.state.pg)
        .await
        .unwrap()
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
async fn publish(env: &Env) -> i64 {
    let epoch = post(env, "/admin/pricing/publish", &json!({})).await["epoch"]
        .as_i64()
        .unwrap();
    env.state.pricebook.replace(
        okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
            .await
            .unwrap(),
    );
    epoch
}
async fn activate(env: &Env, price: i64) -> i64 {
    server_tool_fees::activate(
        env,
        &server_tool_fees::prices(&json!({"billing":"additional","price_per_request_micro":price})),
    )
    .await
}
fn verify_candidate(candidate: &Value, epoch: i64, cap: i64, unit: Option<i64>) {
    assert_eq!(candidate["completion_cap"], cap);
    let estimate = &candidate["estimated_usage"];
    assert_eq!(estimate["completion_tokens"], cap);
    let prompt = estimate["prompt_tokens"].as_i64().unwrap();
    assert!(prompt > 0);
    let base = prompt * 2 + cap * 4;
    let fee = unit.unwrap_or(0) * 5;
    assert_eq!(candidate["pricing_snapshot"]["epoch"], epoch);
    assert_eq!(candidate["pricing_snapshot"]["model_ratio"], 1);
    assert_eq!(candidate["pricing_snapshot"]["completion_ratio"], 2);
    for name in ["amount_micro", "original_amount_micro", "list_price_micro"] {
        assert_eq!(candidate["components"]["base_quote"][name], base);
        assert_eq!(candidate["quote"][name], base + fee);
    }
    assert_eq!(candidate["quote"]["discount_micro"], 0);
    assert_eq!(candidate["components"]["base_quote"]["discount_micro"], 0);
    let tool = &candidate["components"]["server_tool_fees"][0];
    assert_eq!(tool["quantity"], 5);
    assert_eq!(tool["requested"], true);
    assert_eq!(tool["amount_micro"].is_null(), unit.is_none());
    if let Some(unit) = unit {
        assert_eq!(tool["pricing"]["price_per_request_micro"], unit);
        assert_eq!(tool["amount_micro"], fee);
    }
}

#[tokio::test]
async fn native_admission_preserves_quote_components_without_inflating_observed_usage() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            native_case(ingress, stream).await;
        }
    }
}
async fn native_case(ingress: Protocol, stream: bool) {
    let env = setup(Protocol::Anthropic, usage()).await;
    let epoch = activate(&env, 10000).await;
    let response = request_with_tools(
        &env,
        ingress,
        stream,
        matches!(ingress, Protocol::Gemini),
        Some(search()),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert!(!response.text().await.unwrap().contains("upstream_error"));
    let id = server_tool_fees::verify(&env, 21600, epoch).await;
    let snap = snapshot(&env).await;
    assert_eq!(snap["reservation"]["version"], 1, "{id}: {snap}");
    let reservation = &snap["reservation"];
    assert_eq!(reservation["source"], "gateway_admission");
    assert_eq!(reservation["policy"], "max_candidate_amount");
    assert_eq!(reservation["requested_model"], env.model);
    assert_eq!(reservation["max_candidate_index"], 0);
    let candidates = reservation["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["role"], "primary");
    assert_eq!(candidates[0]["routing_model"], env.model);
    assert_eq!(candidates[0]["priced_model"], env.model);
    verify_candidate(&candidates[0], epoch, 512, Some(10000));
    let capture = env.captured.lock().await;
    assert_eq!(
        reservation["reserved_amount_micro"],
        50_000_000 - capture[0].1
    );
    assert_eq!(
        reservation["reserved_amount_micro"],
        candidates[0]["quote"]["amount_micro"]
    );
    assert_eq!(snap["server_tool_fees"][0]["quantity"], 2);
}

#[tokio::test]
async fn gemini_field_styles_preserve_system_input_and_explicit_caps_in_real_holds() {
    for (config, cap, system) in [
        ("generationConfig", "maxOutputTokens", "systemInstruction"),
        (
            "generation_config",
            "max_output_tokens",
            "system_instruction",
        ),
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
            let instruction = "System admission input";
            let mut body = json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]});
            body[config] = json!({cap:16});
            body[system] = json!({"parts":[{"text":instruction}]});
            let response = reqwest::Client::new()
                .post(format!(
                    "http://{}/v1beta/models/{}:{}",
                    env.gateway,
                    env.model,
                    if stream {
                        "streamGenerateContent"
                    } else {
                        "generateContent"
                    }
                ))
                .bearer_auth(&env.token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let snap = snapshot(&env).await;
            let candidate = &snap["reservation"]["candidates"][0];
            assert_eq!(candidate["completion_cap"], 16, "{config}.{cap}");
            let prompt = candidate["estimated_usage"]["prompt_tokens"]
                .as_i64()
                .unwrap();
            // The one-token "hi" control has estimate 8; system input must add to it.
            assert!(prompt > 8, "system input omitted: {candidate}");
            let expected = prompt * 2 + 16 * 4;
            assert_eq!(candidate["quote"]["amount_micro"], expected);
            assert_eq!(snap["reservation"]["reserved_amount_micro"], expected);
            let capture = env.captured.lock().await;
            assert_eq!(50_000_000 - capture[0].1, expected);
            assert_eq!(capture[0].0["max_tokens"], 16);
            assert!(capture[0].0.to_string().contains(instruction));
            drop(capture);
            let row = record(&env).await;
            assert_eq!(row["usage"]["prompt_tokens"], 1000);
            assert_eq!(row["usage"]["completion_tokens"], 50);
            assert_eq!(row["amount_micro"], 1600);
        }
    }
}

async fn primary(env: &Env, available: bool) -> String {
    let primary = format!("{}-primary", env.model);
    post(env, "/admin/models", &json!({"model_name":primary,"model_ratio":"1","completion_ratio":"2","cache_ratio":"0.5","cache_write_ratio":"2",
        "metadata":{"max_output":128},"fallback_models":[env.model],"server_tool_prices":server_tool_fees::prices(&json!({"billing":"additional","price_per_request_micro":1000}))})).await;
    sqlx::query("UPDATE models SET max_output=512 WHERE model_name=$1")
        .bind(&env.model)
        .execute(&env.state.pg)
        .await
        .unwrap();
    if available {
        sqlx::query("UPDATE channels SET models=$2 WHERE name=$1")
            .bind(&env.model)
            .bind(json!([primary, env.model]))
            .execute(&env.state.pg)
            .await
            .unwrap();
        env.state.cand_cache.invalidate_all();
    }
    publish(env).await;
    primary
}
async fn primary_request(
    env: &Env,
    primary: &str,
    stream: bool,
    prefs: Value,
) -> reqwest::Response {
    reqwest::Client::new().post(format!("http://{}/v1/messages",env.gateway)).bearer_auth(&env.token)
        .json(&json!({"model":primary,"messages":[{"role":"user","content":"hi"}],"stream":stream,"provider":prefs,"tools":search()}))
        .send().await.unwrap()
}

#[tokio::test]
async fn all_admitted_candidates_remain_frozen_even_when_primary_serves_or_prices_change() {
    for available in [false, true] {
        for stream in [false, true] {
            frozen_fallback_case(available, stream).await;
        }
    }
}
async fn frozen_fallback_case(available: bool, stream: bool) {
    let gate = Arc::new(CostGate::default());
    let env = Arc::new(
        setup_with_pricing_and_cost_gate(Protocol::Anthropic, usage(), None, Some(gate.clone()))
            .await,
    );
    activate(&env, 10000).await;
    let primary = primary(&env, available).await;
    let epoch = env.state.pricebook.epoch();
    let call = {
        let env = env.clone();
        let model = primary.clone();
        tokio::spawn(async move {
            let response = primary_request(&env, &model, stream, json!({})).await;
            (response.status(), response.text().await.unwrap())
        })
    };
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    let held = env.state.ledger.list_reservations(env.user).await.unwrap();
    assert_eq!(held.len(), 1);
    let updated =
        server_tool_fees::prices(&json!({"billing":"additional","price_per_request_micro":90000}));
    for model in [&primary, &env.model] {
        sqlx::query("UPDATE model_pricing SET model_ratio=9,completion_ratio=9,server_tool_prices=$2 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
            .bind(model).bind(&updated).execute(&env.state.pg).await.unwrap();
        sqlx::query("UPDATE models SET max_output=32768 WHERE model_name=$1")
            .bind(model)
            .execute(&env.state.pg)
            .await
            .unwrap();
    }
    assert!(publish(&env).await > epoch);
    env.state.model_cache.invalidate_all();
    gate.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains("upstream_error"), "{body}");
    let row = record(&env).await;
    let amount = if available { 3600 } else { 21600 };
    assert_eq!(row["amount_micro"], amount);
    let snap = snapshot(&env).await;
    assert_eq!(snap["epoch"], epoch);
    let r = &snap["reservation"];
    let c = r["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0]["routing_model"], primary);
    assert_eq!(c[1]["routing_model"], env.model);
    assert_eq!(c[0]["role"], "primary");
    assert_eq!(c[1]["role"], "fallback");
    verify_candidate(&c[0], epoch, 128, Some(1000));
    verify_candidate(&c[1], epoch, 512, Some(10000));
    assert_eq!(r["max_candidate_index"], 1);
    assert_eq!(r["reserved_amount_micro"], held[0].amount.as_micros());
    assert_eq!(r["reserved_amount_micro"], c[1]["quote"]["amount_micro"]);
    let capture = env.captured.lock().await;
    assert_eq!(
        capture[0].0["max_tokens"],
        if available { 128 } else { 512 }
    );
    drop(capture);
    server_tool_scope::verify_published(
        &env,
        row["request_id"].as_str().unwrap(),
        &json!({"provider":"anthropic","web_search_requests":2,"web_fetch_requests":0}),
        &snap,
        [amount, amount, 0, amount * 1250 / 1000],
    )
    .await;
    let totals = report(&env, "/api/me/stats/breakdown?days=1").await;
    assert_eq!(totals["total"]["tokens"], 1050);
    assert_eq!(totals["total"]["requests"], 1);
}

#[tokio::test]
async fn excluded_fallbacks_are_absent_from_the_actual_hold_evidence() {
    for reason in ["request", "key", "unpriced", "max_price"] {
        let env = setup(Protocol::Anthropic, usage()).await;
        activate(&env, 10000).await;
        let primary = primary(&env, true).await;
        let mut prefs = json!({});
        match reason {
            "request" => prefs = json!({"allow_fallbacks":false}),
            "key" => {
                sqlx::query("UPDATE api_keys SET model_allowlist=$2 WHERE user_id=$1")
                    .bind(env.user)
                    .bind(json!([primary]))
                    .execute(&env.state.pg)
                    .await
                    .unwrap();
                // Console publication invalidates auth separately; the key is already cached by admin APIs.
                redis()
                    .await
                    .del::<(), _>(format!(
                        "auth:key:{}",
                        hex::encode(sha2::Sha256::digest(env.token.as_bytes()))
                    ))
                    .await
                    .unwrap();
            }
            "unpriced" => {
                let ghost = format!("{}-draft", env.model);
                okapi_store::provision::create_model_ratio(&env.state.pg, &ghost, "1", "2", "1")
                    .await
                    .unwrap();
                sqlx::query("UPDATE models SET fallback_models=$2 WHERE model_name=$1")
                    .bind(&primary)
                    .bind(json!([ghost]))
                    .execute(&env.state.pg)
                    .await
                    .unwrap();
            }
            "max_price" => {
                sqlx::query("UPDATE model_pricing SET model_ratio=9 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)").bind(&env.model).execute(&env.state.pg).await.unwrap();
                publish(&env).await;
                prefs = json!({"max_price":{"prompt":3}});
            }
            _ => unreachable!(),
        }
        env.state.model_cache.invalidate_all();
        let response = primary_request(&env, &primary, false, prefs).await;
        assert_eq!(response.status(), 200);
        assert!(!response.text().await.unwrap().contains("upstream_error"));
        let snap = snapshot(&env).await;
        let r = &snap["reservation"];
        let c = r["candidates"].as_array().unwrap();
        assert_eq!(c.len(), 1, "{reason}: {r}");
        assert_eq!(r["max_candidate_index"], 0);
        assert_eq!(c[0]["routing_model"], primary);
        verify_candidate(&c[0], env.state.pricebook.epoch(), 128, Some(1000));
        assert_eq!(r["reserved_amount_micro"], c[0]["quote"]["amount_micro"]);
        let capture = env.captured.lock().await;
        assert_eq!(r["reserved_amount_micro"], 50_000_000 - capture[0].1);
    }
}

#[tokio::test]
async fn plain_unknown_and_explicit_zero_quotes_preserve_their_reservation_semantics() {
    for kind in ["plain", "unknown", "zero"] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
            if kind == "zero" {
                activate(&env, 0).await;
            }
            let response = request_with_tools(
                &env,
                Protocol::Anthropic,
                stream,
                false,
                (kind != "plain").then(search),
            )
            .await;
            if kind == "unknown" {
                // 声明了工具却没给工具定价：准入即拒，没有预扣、没有账单
                assert_eq!(response.status(), 400);
                assert!(
                    response
                        .text()
                        .await
                        .unwrap()
                        .contains("server_tool_unpriced")
                );
                continue;
            }
            assert_eq!(response.status(), 200);
            assert!(!response.text().await.unwrap().contains("upstream_error"));
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 1600);
            let snap = snapshot(&env).await;
            let r = &snap["reservation"];
            let c = &r["candidates"][0];
            assert_eq!(r["candidates"].as_array().unwrap().len(), 1);
            if kind == "plain" {
                assert!(
                    c["components"]["server_tool_fees"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
            } else {
                verify_candidate(
                    c,
                    env.state.pricebook.epoch(),
                    512,
                    (kind == "zero").then_some(0),
                );
            }
            assert_eq!(r["reserved_amount_micro"], c["quote"]["amount_micro"]);
            let capture = env.captured.lock().await;
            assert_eq!(r["reserved_amount_micro"], 50_000_000 - capture[0].1);
            let cost: Option<i64> = sqlx::query_scalar(
                "SELECT upstream_cost_micro FROM billing_records WHERE request_id::text=$1",
            )
            .bind(row["request_id"].as_str().unwrap())
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
            assert_eq!(cost.is_none(), kind == "unknown");
        }
    }
}

#[tokio::test]
async fn failed_admitted_requests_keep_only_estimate_metadata_and_zero_money() {
    for failure in ["capability", "missing_usage"] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, anthropic_usage::fixture()).await;
            let epoch = activate(&env, 10000).await;
            if failure == "capability" {
                sqlx::query("UPDATE channels SET capabilities=$2 WHERE name=$1")
                    .bind(&env.model)
                    .bind(json!({"tools":false}))
                    .execute(&env.state.pg)
                    .await
                    .unwrap();
            }
            let response =
                request_with_tools(&env, Protocol::Anthropic, stream, false, Some(search())).await;
            assert_eq!(
                response.status(),
                if failure == "capability" {
                    503
                } else if stream {
                    200
                } else {
                    502
                }
            );
            assert!(
                response
                    .text()
                    .await
                    .unwrap()
                    .contains(if failure == "capability" {
                        "no_available_channel"
                    } else {
                        "upstream_error"
                    })
            );
            let row = record(&env).await;
            let id = row["request_id"].as_str().unwrap();
            let stored:(i64,i64,i64,Option<i64>,Value)=sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot FROM billing_records WHERE request_id::text=$1").bind(id).fetch_one(&env.state.pg).await.unwrap();
            assert_eq!((stored.0, stored.1, stored.2, stored.3), (0, 0, 0, None));
            assert_eq!(stored.4.as_object().unwrap().len(), 2);
            assert_eq!(stored.4["epoch"], epoch);
            verify_candidate(
                &stored.4["reservation"]["candidates"][0],
                epoch,
                512,
                Some(10000),
            );
            assert!(stored.4["server_tool_fees"].is_null());
            let event:Value=sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1").bind(id).fetch_one(&env.state.pg).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
                stored.4
            );
            assert!(event["server_tool_usage"].is_null());
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

#[tokio::test]
async fn journal_replay_preserves_frozen_reservation_and_actual_charge_once() {
    for stream in [false, true] {
        journal_case(stream).await;
    }
}
async fn journal_case(stream: bool) {
    let env = setup(Protocol::Anthropic, usage()).await;
    let epoch = activate(&env, 10000).await;
    let rule = format!("reservation_no_success_{}", env.user);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_records ADD CONSTRAINT {rule} CHECK(user_id<>{} OR status<>20)",
        env.user
    )))
    .execute(&env.state.pg)
    .await
    .unwrap();
    let response =
        request_with_tools(&env, Protocol::Anthropic, stream, false, Some(search())).await;
    let status = response.status();
    let body = response.text().await.unwrap();
    let held = env.state.ledger.list_reservations(env.user).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1 AND status=20")
            .bind(env.user)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_records DROP CONSTRAINT {rule}"
    )))
    .execute(&env.state.pg)
    .await
    .unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains("upstream_error"), "{body}");
    assert_eq!(count, 0);
    assert_eq!(held.len(), 1);
    let payload: String = redis()
        .await
        .hget(
            "settlement:{retry}:payloads",
            held[0].request_id.to_string(),
        )
        .await
        .unwrap();
    let saved: okapi_ledger::pg::OwnedSettlementInput = serde_json::from_str(&payload).unwrap();
    assert_eq!(saved.amount.as_micros(), 21600);
    assert_eq!(saved.pricing_epoch, Some(epoch));
    let frozen = saved.pricing_snapshot.as_ref().unwrap();
    assert_eq!(
        frozen["reservation"]["reserved_amount_micro"],
        held[0].amount.as_micros()
    );
    verify_candidate(
        &frozen["reservation"]["candidates"][0],
        epoch,
        512,
        Some(10000),
    );
    let expected = frozen.clone();
    sqlx::query("UPDATE model_pricing SET model_ratio=9 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)").bind(&env.model).execute(&env.state.pg).await.unwrap();
    assert!(publish(&env).await > epoch);
    assert!(env.state.settle_success(saved.as_input()).await.unwrap());
    assert!(!env.state.settle_success(saved.as_input()).await.unwrap());
    let row = record(&env).await;
    assert_eq!(row["amount_micro"], 21600);
    assert_eq!(snapshot(&env).await, expected);
    server_tool_scope::verify_published(
        &env,
        row["request_id"].as_str().unwrap(),
        &json!({"provider":"anthropic","web_search_requests":2,"web_fetch_requests":0}),
        &expected,
        [21600, 21600, 0, 27000],
    )
    .await;
    assert!(
        env.state
            .ledger
            .list_reservations(env.user)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - 21600
    );
    let totals = report(&env, "/api/me/stats/breakdown?days=1").await;
    assert_eq!(totals["total"]["tokens"], 1050);
    assert_eq!(totals["total"]["requests"], 1);
}
