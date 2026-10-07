use super::*;

async fn order(env: &TestEnv, gateway: &str) -> Value {
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/me/topup", env.addr))
        .bearer_auth(&env.user_token)
        .json(&json!({"amount_micro":5_000_000,"gateway":gateway}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
fn epay_fields(order: &Value) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("pid", "1001".to_owned()),
        (
            "trade_no",
            format!("trade-{}", order["order_no"].as_str().unwrap()),
        ),
        (
            "out_trade_no",
            order["order_no"].as_str().unwrap().to_owned(),
        ),
        (
            "money",
            order["params"]["money"]
                .as_str()
                .unwrap_or("5.00")
                .to_owned(),
        ),
        ("trade_status", "TRADE_SUCCESS".to_owned()),
    ])
}
fn epay_url(env: &TestEnv, fields: &BTreeMap<&str, String>) -> reqwest::Url {
    let mut url = reqwest::Url::parse(&format!("http://{}/pay/callback/epay", env.addr)).unwrap();
    url.query_pairs_mut()
        .extend_pairs(fields)
        .append_pair("sign", &epay_sign(fields))
        .append_pair("sign_type", "MD5");
    url
}
async fn unpaid(env: &TestEnv, order: &Value) {
    let status: i16 = sqlx::query_scalar("SELECT status FROM recharge_orders WHERE order_no=$1")
        .bind(order["order_no"].as_str().unwrap())
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 0, "invalid callback changed the business source");
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        0
    );
    let intents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fund_transfers WHERE user_id=$1")
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(
        intents, 0,
        "invalid callback queued money for later delivery"
    );
}
fn event(order: &Value, kind: &str) -> Value {
    json!({"id":format!("evt_{}",Uuid::new_v4()),"type":kind,
        "data":{"object":{"id":order["session_id"],"object":"checkout.session","mode":"payment",
        "status":"complete","payment_status":"paid","amount_total":500,"currency":"usd",
        "metadata":{"order_no":order["order_no"]}}}})
}
fn signature(body: &str, ts: i64) -> String {
    let mut mac = <Hmac<Sha256>>::new_from_slice(STRIPE_WH.as_bytes()).unwrap();
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
}
async fn stripe(env: &TestEnv, event: &Value, ts: i64) -> reqwest::Response {
    let body = event.to_string();
    reqwest::Client::new()
        .post(format!("http://{}/pay/callback/stripe", env.addr))
        .header("stripe-signature", signature(&body, ts))
        .body(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn signed_epay_amount_and_merchant_must_match_the_quote() {
    let env = setup().await;
    for (field, value) in [
        ("money", "0.01"),
        ("money", "35.01"),
        ("pid", "other-merchant"),
    ] {
        let order = order(&env, "epay").await;
        let mut fields = epay_fields(&order);
        fields.insert(field, value.to_owned());
        let response = reqwest::Client::new()
            .get(epay_url(&env, &fields))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            400,
            "signed mismatched {field} was accepted"
        );
        unpaid(&env, &order).await;
    }
}

#[tokio::test]
async fn epay_cannot_settle_a_stripe_order() {
    let env = setup().await;
    let order = order(&env, "stripe").await;
    let response = reqwest::Client::new()
        .get(epay_url(&env, &epay_fields(&order)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    unpaid(&env, &order).await;
}

#[tokio::test]
async fn epay_decodes_form_values_and_signs_additional_notification_fields() {
    let env = setup().await;
    let order = order(&env, "epay").await;
    let mut fields = epay_fields(&order);
    fields.insert("name", "A+B & 测试".to_owned());
    fields.insert("buyer", "buyer+one@example.test".to_owned());
    let response = reqwest::Client::new()
        .get(epay_url(&env, &fields))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "success");
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn epay_rejects_duplicate_fields_and_missing_transaction_identity() {
    let env = setup().await;
    let order = order(&env, "epay").await;
    let mut url = epay_url(&env, &epay_fields(&order));
    url.query_pairs_mut().append_pair("money", "35.00");
    assert_eq!(
        reqwest::Client::new()
            .get(url)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    unpaid(&env, &order).await;
    let mut fields = epay_fields(&order);
    fields.remove("trade_no");
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&env, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    unpaid(&env, &order).await;
}

#[tokio::test]
async fn a_payment_transaction_cannot_credit_two_users_concurrently() {
    let first = setup().await;
    let second = setup().await;
    let trade = format!("same-{}", Uuid::new_v4());
    let mut calls = Vec::new();
    let start = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    for env in [&first, &second] {
        let order = order(env, "epay").await;
        let mut fields = epay_fields(&order);
        fields.insert("trade_no", trade.clone());
        let url = epay_url(env, &fields);
        let start = start.clone();
        calls.push(tokio::spawn(async move {
            start.wait().await;
            reqwest::Client::new()
                .get(url)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut statuses = Vec::new();
    for call in calls {
        statuses.push(call.await.unwrap());
    }
    statuses.sort_unstable();
    assert_eq!(statuses, vec![200, 409]);
    let total = first
        .ledger
        .balance(first.user_id)
        .await
        .unwrap()
        .checked_add(second.ledger.balance(second.user_id).await.unwrap())
        .unwrap();
    assert_eq!(total.as_micros(), 5_000_000);
}

#[tokio::test]
async fn a_paid_order_cannot_be_rebound_to_a_different_transaction() {
    let env = setup().await;
    let order = order(&env, "epay").await;
    let mut fields = epay_fields(&order);
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&env, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    fields.insert("trade_no", format!("other-{}", Uuid::new_v4()));
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&env, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn stripe_completed_unpaid_does_not_credit_before_async_success() {
    let env = setup().await;
    let order = order(&env, "stripe").await;
    let mut pending = event(&order, "checkout.session.completed");
    pending["data"]["object"]["payment_status"] = json!("unpaid");
    assert_eq!(
        stripe(&env, &pending, chrono::Utc::now().timestamp())
            .await
            .status(),
        200
    );
    unpaid(&env, &order).await;
    let paid = event(&order, "checkout.session.async_payment_succeeded");
    for _ in 0..2 {
        assert_eq!(
            stripe(&env, &paid, chrono::Utc::now().timestamp())
                .await
                .status(),
            200
        );
    }
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn stripe_async_success_can_be_the_first_delivered_event() {
    let env = setup().await;
    let order = order(&env, "stripe").await;
    assert_eq!(
        stripe(
            &env,
            &event(&order, "checkout.session.async_payment_succeeded"),
            chrono::Utc::now().timestamp()
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn signed_stripe_fields_must_match_a_paid_checkout_and_the_order() {
    let env = setup().await;
    for (field, value) in [
        ("amount_total", json!(1)),
        ("currency", json!("cny")),
        ("id", json!("cs_other")),
        ("mode", json!("setup")),
        ("status", json!("open")),
    ] {
        let order = order(&env, "stripe").await;
        let mut paid = event(&order, "checkout.session.completed");
        paid["data"]["object"][field] = value;
        assert_eq!(
            stripe(&env, &paid, chrono::Utc::now().timestamp())
                .await
                .status(),
            400,
            "mismatched {field} accepted"
        );
        unpaid(&env, &order).await;
    }
}

#[tokio::test]
async fn stripe_cannot_settle_an_epay_order() {
    let env = setup().await;
    let order = order(&env, "epay").await;
    let mut paid = event(&order, "checkout.session.completed");
    paid["data"]["object"]["id"] = json!("cs_wrong_gateway");
    assert_eq!(
        stripe(&env, &paid, chrono::Utc::now().timestamp())
            .await
            .status(),
        400
    );
    unpaid(&env, &order).await;
}

#[tokio::test]
async fn valid_but_stale_or_future_stripe_signatures_do_not_credit() {
    let env = setup().await;
    for timestamp in [
        chrono::Utc::now().timestamp() - 600,
        chrono::Utc::now().timestamp() + 600,
        i64::MIN,
        i64::MAX,
    ] {
        let order = order(&env, "stripe").await;
        assert_eq!(
            stripe(
                &env,
                &event(&order, "checkout.session.completed"),
                timestamp
            )
            .await
            .status(),
            400
        );
        unpaid(&env, &order).await;
    }
}

#[tokio::test]
async fn any_valid_stripe_v1_signature_is_accepted_during_secret_rotation() {
    let env = setup().await;
    let order = order(&env, "stripe").await;
    let paid = event(&order, "checkout.session.completed");
    let body = paid.to_string();
    let sig = format!(
        "{},v1={}",
        signature(&body, chrono::Utc::now().timestamp()),
        "00".repeat(32)
    );
    let response = reqwest::Client::new()
        .post(format!("http://{}/pay/callback/stripe", env.addr))
        .header("stripe-signature", sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn epay_amount_parser_never_rounds_or_accepts_non_decimal_values() {
    let env = setup().await;
    let order = order(&env, "epay").await;
    let mut fields = epay_fields(&order);
    for money in [
        "NaN",
        "3.5e1",
        "+35.00",
        "-35.00",
        "35.001",
        "0",
        "",
        "999999999999999999999999.00",
    ] {
        fields.insert("money", money.to_owned());
        assert_eq!(
            reqwest::Client::new()
                .get(epay_url(&env, &fields))
                .send()
                .await
                .unwrap()
                .status(),
            400,
            "{money}"
        );
        unpaid(&env, &order).await;
    }
    // Lossless normalization is accepted; settlement still uses the stored USD amount.
    fields.insert("money", "35.0".to_owned());
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&env, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn new_stripe_order_requires_persisted_session_before_acknowledging_payment() {
    let env = setup().await;
    let order = order(&env, "stripe").await;
    let no = order["order_no"].as_str().unwrap();
    sqlx::query("UPDATE recharge_orders SET checkout_session_id=NULL WHERE order_no=$1")
        .bind(no)
        .execute(&env.pg)
        .await
        .unwrap();
    let paid = event(&order, "checkout.session.completed");
    assert_eq!(
        stripe(&env, &paid, chrono::Utc::now().timestamp())
            .await
            .status(),
        503
    );
    unpaid(&env, &order).await;
    assert!(
        okapi_store::payments::bind_checkout(&env.pg, no, order["session_id"].as_str().unwrap())
            .await
            .unwrap()
    );
    assert_eq!(
        stripe(&env, &paid, chrono::Utc::now().timestamp())
            .await
            .status(),
        200
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn legacy_pending_order_is_validated_and_old_paid_transaction_cannot_be_reused() {
    let first = setup().await;
    let old = order(&first, "epay").await;
    let mut fields = epay_fields(&old);
    sqlx::query(
        "UPDATE recharge_orders SET payment_contract_version=0,merchant_id=NULL WHERE order_no=$1",
    )
    .bind(old["order_no"].as_str().unwrap())
    .execute(&first.pg)
    .await
    .unwrap();
    fields.insert("money", "0.01".to_owned());
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&first, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    unpaid(&first, &old).await;
    fields.insert("money", "35.00".to_owned());
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&first, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // Simulate a historical paid row from before payment_receipts existed.
    sqlx::query("DELETE FROM payment_receipts WHERE order_id=(SELECT id FROM recharge_orders WHERE order_no=$1)")
        .bind(old["order_no"].as_str().unwrap()).execute(&first.pg).await.unwrap();
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&first, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let second = setup().await;
    let new = order(&second, "epay").await;
    fields.insert("out_trade_no", new["order_no"].as_str().unwrap().to_owned());
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&second, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    unpaid(&second, &new).await;
    assert_eq!(
        first
            .ledger
            .balance(first.user_id)
            .await
            .unwrap()
            .as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn legacy_stripe_without_session_binding_still_requires_exact_quote() {
    let env = setup().await;
    let order = order(&env, "stripe").await;
    sqlx::query("UPDATE recharge_orders SET payment_contract_version=0,checkout_session_id=NULL WHERE order_no=$1")
        .bind(order["order_no"].as_str().unwrap()).execute(&env.pg).await.unwrap();
    let mut paid = event(&order, "checkout.session.completed");
    paid["data"]["object"]["amount_total"] = json!(1);
    assert_eq!(
        stripe(&env, &paid, chrono::Utc::now().timestamp())
            .await
            .status(),
        400
    );
    unpaid(&env, &order).await;
    paid["data"]["object"]["amount_total"] = json!(500);
    assert_eq!(
        stripe(&env, &paid, chrono::Utc::now().timestamp())
            .await
            .status(),
        200
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn changed_exchange_rate_does_not_requote_an_existing_order() {
    let env = setup().await;
    let order = order(&env, "epay").await;
    sqlx::query("UPDATE settings SET value=jsonb_set(value,'{usd_to_cny_milli}','8000') WHERE key='payment_epay'")
        .execute(&env.pg).await.unwrap();
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&env, &epay_fields(&order)))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
}

#[tokio::test]
async fn current_merchant_cannot_replace_the_orders_original_merchant() {
    let env = setup().await;
    let order = order(&env, "epay").await;
    sqlx::query(
        "UPDATE settings SET value=jsonb_set(value,'{pid}','\"1002\"') WHERE key='payment_epay'",
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let mut fields = epay_fields(&order);
    fields.insert("pid", "1002".to_owned());
    assert_eq!(
        reqwest::Client::new()
            .get(epay_url(&env, &fields))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    unpaid(&env, &order).await;
}

#[tokio::test]
async fn stripe_checks_raw_body_and_rejects_ambiguous_signature_timestamps() {
    let env = setup().await;
    let order = order(&env, "stripe").await;
    let body = event(&order, "checkout.session.completed").to_string();
    let ts = chrono::Utc::now().timestamp();
    let sig = signature(&body, ts);
    for (sent, header) in [
        (format!("{body}\n"), sig.clone()),
        (body.clone(), format!("{sig},t={ts}")),
    ] {
        let response = reqwest::Client::new()
            .post(format!("http://{}/pay/callback/stripe", env.addr))
            .header("stripe-signature", header)
            .body(sent)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        unpaid(&env, &order).await;
    }
}

#[tokio::test]
async fn malformed_checkout_response_never_returns_a_successful_payment_link() {
    let env = setup().await;
    for fixture in [
        json!({}),
        json!({"id":"cs_bad","url":"javascript:alert(1)"}),
        json!({"id":"","url":"https://checkout.stripe.test"}),
    ] {
        let app = axum::Router::new().route(
            "/v1/checkout/sessions",
            post(move || async move { axum::Json(fixture) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        sqlx::query("UPDATE settings SET value=jsonb_set(value,'{api_base}',$1::jsonb) WHERE key='payment_stripe'")
            .bind(json!(format!("http://{addr}"))).execute(&env.pg).await.unwrap();
        let response = reqwest::Client::new()
            .post(format!("http://{}/api/me/topup", env.addr))
            .bearer_auth(&env.user_token)
            .json(&json!({"amount_micro":5_000_000,"gateway":"stripe"}))
            .send()
            .await
            .unwrap();
        server.abort();
        assert_eq!(response.status(), 502);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "payment_gateway_error"
        );
    }
    let paid:i64=sqlx::query_scalar("SELECT count(*) FROM recharge_orders WHERE user_id=$1 AND (status<>0 OR checkout_session_id IS NOT NULL)")
        .bind(env.user_id).fetch_one(&env.pg).await.unwrap();
    assert_eq!(paid, 0);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        0
    );
}

/// 下单有频控与未支付上限：Stripe 每单都要调一次外部 API，未支付订单又不会自动作废，
/// 不设限时任何登录用户都能刷商户 API、往订单表里灌单。
#[tokio::test]
async fn order_placement_is_rate_limited_and_caps_unpaid_orders() {
    let env = setup().await;
    let place = || {
        reqwest::Client::new()
            .post(format!("http://{}/api/me/topup", env.addr))
            .bearer_auth(&env.user_token)
            .json(&json!({"amount_micro":5_000_000,"gateway":"epay"}))
            .send()
    };
    for _ in 0..10 {
        assert_eq!(place().await.unwrap().status(), 200);
    }
    let limited = place().await.unwrap();
    assert_eq!(limited.status(), 429);
    let body: Value = limited.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited", "{body}");
    assert_eq!(body["error"]["param"], "place_order", "{body}");

    // 换个新用户，直接把未支付订单补到上限：频控之外还有总量闸
    let other = setup().await;
    sqlx::query(
        "INSERT INTO recharge_orders(order_no,user_id,amount_micro,gateway)
         SELECT 'cap-'||gen_random_uuid()::text,$1,5000000,'epay' FROM generate_series(1,20)",
    )
    .bind(other.user_id)
    .execute(&other.pg)
    .await
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/me/topup", other.addr))
        .bearer_auth(&other.user_token)
        .json(&json!({"amount_micro":5_000_000,"gateway":"epay"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "too_many_pending_orders", "{body}");
    let unpaid: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM recharge_orders WHERE user_id=$1 AND status=0")
            .bind(other.user_id)
            .fetch_one(&other.pg)
            .await
            .unwrap();
    assert_eq!(unpaid, 20, "被拒的下单不能留下订单行");
}
