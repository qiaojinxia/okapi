//! Real HTTP admission, exact provider usage billing and durable task parity.
use super::*;

pub(super) async fn token_env() -> Env {
    let env = setup().await;
    sqlx::query("UPDATE model_pricing SET pricing_mode='ratio',model_ratio=2.5,completion_ratio=6,image_ratio=1.6 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&env.model).execute(&env.state.pg).await.unwrap();
    sqlx::query("UPDATE models SET max_output=1000 WHERE model_name=$1")
        .bind(&env.model)
        .execute(&env.state.pg)
        .await
        .unwrap();
    published_pricing::publish(&env.state.pg, env.user).await;
    let book = gateway::pricing_loader::load_pricebook(&env.state.pg)
        .await
        .unwrap();
    env.state.pricebook.replace(book);
    env
}

pub(super) fn usage(text: u32, image: u32, output: u32) -> Value {
    json!({"input_tokens":text+image,"output_tokens":output,"total_tokens":text+image+output,
        "input_tokens_details":{"text_tokens":text,"image_tokens":image}})
}

pub(super) fn response(usage: Value, count: usize) -> Value {
    let mut body = json!({"created":1_700_000_000,"data":(0..count)
        .map(|_|json!({"b64_json":"iVBORw0KGgo="})).collect::<Vec<_>>()});
    body["usage"] = usage;
    body
}

pub(super) async fn assert_usage(env: &Env, record: &Value, text: u32, image: u32, output: u32) {
    assert_eq!(record["prompt_tokens"], text + image);
    assert_eq!(record["completion_tokens"], output);
    assert_eq!(record["pricing_snapshot"]["image_usage_reported"], true);
    assert_eq!(
        record["pricing_snapshot"]["image_usage"],
        json!({
            "input_text_tokens":text,"input_image_tokens":image,"output_image_tokens":output
        })
    );
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1 ORDER BY id DESC LIMIT 1")
        .bind(record["request_id"].as_str().unwrap())
        .fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(payload["prompt_tokens"], text + image);
    assert_eq!(payload["completion_tokens"], output);
    for field in ["amount_micro", "original_amount_micro", "discount_micro"] {
        assert_eq!(payload[field], record[field]);
    }
    assert_eq!(
        payload["upstream_cost_micro"],
        record["upstream_cost_micro"]
    );
    let snapshot: Value =
        serde_json::from_str(payload["ratio_snapshot"].as_str().unwrap()).unwrap();
    assert_eq!(snapshot, record["pricing_snapshot"]);
}

#[tokio::test]
async fn ratio_generations_bill_usage_once_for_multiple_images() {
    let mut env = token_env().await;
    let call = launch(env.request(false).json(&env.body(3)));
    env.peer()
        .await
        .raw(200, response(usage(20, 80, 200), 2).to_string());
    let reply = finish(call, 200).await;
    let record = env.record(&reply).await;
    assert_usage(&env, &record, 20, 80, 200).await;
    // 20 text * 5 + 80 input image * 8 + 200 output image * 30 micro.
    env.assert_money(6740, 1).await;
    assert_eq!(record["pricing_snapshot"]["mode"], "ratio");
    assert_eq!(record["pricing_snapshot"]["media_units"], 2);
    assert_eq!(record["original_amount_micro"], 6740);
    assert_eq!(record["discount_micro"], 0);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["usage"],
        usage(20, 80, 200)
    );
}

#[tokio::test]
async fn per_image_pricing_keeps_provider_tokens_for_analytics() {
    let mut env = setup().await;
    let call = launch(env.request(false).json(&env.body(3)));
    env.peer()
        .await
        .raw(200, response(usage(20, 80, 200), 2).to_string());
    let reply = finish(call, 200).await;
    assert_usage(&env, &env.record(&reply).await, 20, 80, 200).await;
    env.assert_money(PRICE * 2, 1).await;
}

#[tokio::test]
async fn per_image_pricing_also_obeys_token_admission_limits() {
    let env = setup().await;
    sqlx::query("UPDATE api_keys SET tpm_limit=1 WHERE id=$1")
        .bind(env.key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let reply = env.request(false).json(&env.body(1)).send().await.unwrap();
    assert_eq!(reply.status(), 429);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "rate_limited"
    );
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn token_edits_support_json_and_multipart_without_counting_base64_as_text() {
    let mut env = token_env().await;
    let mut body = env.body(1);
    body["images"] = json!([{"image_url":"data:image/png;base64,iVBORw0KGgo="}]);
    let call = launch(env.request(true).json(&body));
    env.peer()
        .await
        .raw(200, response(usage(20, 80, 200), 1).to_string());
    let reply = finish(call, 200).await;
    assert_usage(&env, &env.record(&reply).await, 20, 80, 200).await;
    let call = launch(env.request(true).multipart(env.form()));
    env.peer()
        .await
        .raw(200, response(usage(20, 80, 200), 1).to_string());
    let reply = finish(call, 200).await;
    assert_usage(&env, &env.record(&reply).await, 20, 80, 200).await;
    env.assert_money(13480, 2).await;
}

#[tokio::test]
async fn missing_or_invalid_usage_refunds_without_retrying_generation() {
    let mut env = token_env().await;
    for bad in [
        Value::Null,
        json!({}),
        usage(0, 0, 0),
        json!({"input_tokens":100,"output_tokens":200,"input_tokens_details":{"text_tokens":10,"image_tokens":80}}),
        json!({"input_tokens":100,"output_tokens":200,"total_tokens":301,"input_tokens_details":{"text_tokens":20,"image_tokens":80}}),
        json!({"input_tokens":100,"output_tokens":-1,"input_tokens_details":{"text_tokens":20,"image_tokens":80}}),
        json!({"input_tokens":100,"output_tokens":2_147_483_648_u64,"input_tokens_details":{"text_tokens":20,"image_tokens":80}}),
    ] {
        let is_zero = bad == usage(0, 0, 0);
        let before = env.hits.load(Ordering::SeqCst);
        let call = launch(env.request(false).json(&env.body(1)));
        env.peer().await.raw(200, response(bad, 1).to_string());
        let reply = finish(call, if is_zero { 200 } else { 502 }).await;
        reply.bytes().await.unwrap();
        assert_eq!(env.hits.load(Ordering::SeqCst), before + 1);
    }
    // Explicit reported zero is valid, missing/invalid usage is never a free success.
    env.assert_money(0, 1).await;
}

#[tokio::test]
async fn insufficient_balance_and_tpm_reject_before_upstream() {
    let mut env = token_env().await;
    sqlx::query("UPDATE api_keys SET tpm_limit=1 WHERE id=$1")
        .bind(env.key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let reply = env.request(false).json(&env.body(1)).send().await.unwrap();
    assert_eq!(reply.status(), 429);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "rate_limited"
    );
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
    // An estimate exceeding available quota must not dispatch either.
    sqlx::query("UPDATE api_keys SET tpm_limit=NULL WHERE id=$1")
        .bind(env.key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    // Fresh key lookup avoids the successful-auth cache of the previous call.
    env.state
        .sched
        .auth_del(&hex::encode(Sha256::digest(env.token.as_bytes())))
        .await;
    sqlx::query("UPDATE models SET max_output=100000 WHERE model_name=$1")
        .bind(&env.model)
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state.model_cache.invalidate_all();
    let reply = env.request(false).json(&env.body(1)).send().await.unwrap();
    assert_eq!(reply.status(), 429);
    assert_eq!(
        reply.json::<Value>().await.unwrap()["error"]["code"],
        "insufficient_quota"
    );
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
    assert!(
        timeout(Duration::from_millis(50), env.incoming.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn pricing_remains_frozen_while_provider_is_running() {
    let mut env = token_env().await;
    let call = launch(env.request(false).json(&env.body(1)));
    let peer = env.peer().await;
    sqlx::query("UPDATE model_pricing SET model_ratio=25 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&env.model).execute(&env.state.pg).await.unwrap();
    published_pricing::publish(&env.state.pg, env.user).await;
    env.state.pricebook.replace(
        gateway::pricing_loader::load_pricebook(&env.state.pg)
            .await
            .unwrap(),
    );
    peer.raw(200, response(usage(20, 80, 200), 1).to_string());
    let reply = finish(call, 200).await;
    assert_usage(&env, &env.record(&reply).await, 20, 80, 200).await;
    env.assert_money(6740, 1).await;
    let call = launch(env.request(false).json(&env.body(1)));
    env.peer()
        .await
        .raw(200, response(usage(20, 80, 200), 1).to_string());
    finish(call, 200).await.bytes().await.unwrap();
    env.assert_money(74140, 2).await;
}

#[tokio::test]
async fn duplicate_and_unrepresentable_usage_never_silently_changes_the_bill() {
    let mut env = token_env().await;
    for raw in [
        r#"{"data":[{"b64_json":"AA=="}],"usage":{"input_tokens":1,"input_tokens":2,"output_tokens":1,"input_tokens_details":{"text_tokens":2,"image_tokens":0}}}"#,
        r#"{"data":[{"b64_json":"AA=="}],"usage":{"input_tokens":1,"output_tokens":1.5,"input_tokens_details":{"text_tokens":1,"image_tokens":0}}}"#,
        r#"{"data":[{"b64_json":"AA=="}],"usage":{"input_tokens":2,"output_tokens":1,"input_tokens_details":{"text_tokens":1,"image_tokens":1,"cached_tokens":1}}}"#,
        r#"{"data":[{"b64_json":"AA=="}],"usage":{"input_tokens":1,"output_tokens":1,"input_tokens_details":{"text_tokens":1,"image_tokens":0}},"usage":null}"#,
    ] {
        let call = launch(env.request(false).json(&env.body(1)));
        env.peer().await.raw(200, raw.to_owned());
        finish(call, 502).await.bytes().await.unwrap();
        env.assert_money(0, 0).await;
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn durable_async_images_preserve_usage_and_charge_only_once() {
    durable_usage_case(false).await;
}

#[tokio::test]
async fn durable_async_images_preserve_cache_intersections_and_charge_only_once() {
    durable_usage_case(true).await;
}

async fn durable_usage_case(cached: bool) {
    let mut env = if cached {
        super::cache_billing::cache_env().await
    } else {
        token_env().await
    };
    let reported = if cached {
        super::cache_billing::cached_usage(200)
    } else {
        usage(20, 80, 200)
    };
    let expected = if cached { 6462 } else { 6740 };
    env.state
        .settings_cache
        .insert("image_tasks_enabled".into(), Arc::new(Some(json!(true))))
        .await;
    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/images/generations/async", env.address);
    let task: Value = client
        .post(&url)
        .bearer_auth(&env.token)
        .header("idempotency-key", "token-task")
        .json(&env.body(3))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(task["status"], "queued", "{task}");
    let state = env.state.clone();
    let worker = tokio::spawn(async move { gateway::images::tasks::run_one(&state).await });
    env.peer()
        .await
        .raw(200, response(reported.clone(), 2).to_string());
    assert!(timeout(WAIT, worker).await.unwrap().unwrap().unwrap());
    let poll = format!(
        "http://{}{}",
        env.address,
        task["poll_url"].as_str().unwrap()
    );
    let result: Value = client
        .get(&poll)
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(result["result"]["usage"], reported);
    let record: Value =
        sqlx::query_scalar("SELECT to_jsonb(b) FROM billing_records b WHERE user_id=$1")
            .bind(env.user)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_usage(&env, &record, 20, 80, 200).await;
    if cached {
        super::cache_billing::assert_cache(&env, &record, 1, 200).await;
    }
    env.assert_money(expected, 1).await;
    let duplicate: Value = client
        .post(&url)
        .bearer_auth(&env.token)
        .header("idempotency-key", "token-task")
        .json(&env.body(3))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(duplicate["id"], task["id"]);
    assert_eq!(duplicate["status"], "completed");
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
    env.assert_money(expected, 1).await;
}

#[tokio::test]
async fn token_pricing_preserves_group_user_rules_cost_and_monthly_usage() {
    let mut env = token_env().await;
    sqlx::query("UPDATE users SET price_multiplier=0.5 WHERE id=$1")
        .bind(env.user)
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET upstream_unit_cost=jsonb_build_object('relative_cost_milli',2000) WHERE id=ANY($1)")
        .bind(&env.channels)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let rows = okapi_store::pricing::load_pricing_source_rows(&env.state.pg)
        .await
        .unwrap();
    let mut source = gateway::pricing_loader::build_source(&rows);
    source
        .groups
        .iter_mut()
        .find(|group| group.group.as_str() == "default")
        .unwrap()
        .ratio = "2".parse().unwrap();
    source.rules.push(okapi_pricing::PricingRule {
        code: "image-volume".into(),
        kind: okapi_pricing::RuleKind::Volume {
            min_monthly_tokens: 0,
            min_monthly_spend_micro: 0,
        },
        multiplier: "0.5".parse().unwrap(),
        scope: okapi_pricing::RuleScope::default(),
        priority: 0,
        stacking: okapi_pricing::Stacking::Stackable,
        valid_from: None,
        valid_to: None,
    });
    env.state
        .pricebook
        .replace(okapi_pricing::book::compile(source).unwrap());
    let call = launch(env.request(false).json(&env.body(2)));
    env.peer()
        .await
        .raw(200, response(usage(20, 80, 200), 2).to_string());
    let reply = finish(call, 200).await;
    let record = env.record(&reply).await;
    assert_usage(&env, &record, 20, 80, 200).await;
    env.assert_money(3370, 1).await;
    assert_eq!(record["original_amount_micro"], 13480);
    assert_eq!(record["discount_micro"], 10110);
    assert_eq!(record["upstream_cost_micro"], 13480);
    assert_eq!(env.state.sched.monthly_tokens_get(env.user).await, 300);
    assert_eq!(
        record["pricing_snapshot"]["rules"][0]["code"],
        "image-volume"
    );
}

#[tokio::test]
async fn tiered_images_use_actual_response_total_for_price_band() {
    let mut env = token_env().await;
    sqlx::query("UPDATE model_pricing SET pricing_mode='tiered',tier_expr='0:5,250:10' WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&env.model).execute(&env.state.pg).await.unwrap();
    published_pricing::publish(&env.state.pg, env.user).await;
    env.state.pricebook.replace(
        gateway::pricing_loader::load_pricebook(&env.state.pg)
            .await
            .unwrap(),
    );
    let call = launch(env.request(false).json(&env.body(1)));
    env.peer()
        .await
        .raw(200, response(usage(20, 80, 200), 1).to_string());
    let reply = finish(call, 200).await;
    assert_eq!(
        env.record(&reply).await["pricing_snapshot"]["mode"],
        "tiered"
    );
    env.assert_money(13480, 1).await;
}
