use super::*;
use fred::interfaces::{HashesInterface, KeysInterface};

#[tokio::test]
async fn paid_callback_survives_redis_failure_without_another_payment() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let order: Value = client
        .post(format!("http://{}/api/me/topup", env.addr))
        .bearer_auth(&env.user_token)
        .json(&json!({"amount_micro": 5_000_000, "gateway": "epay"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let order_no = order["order_no"].as_str().unwrap();
    let params = BTreeMap::from([
        ("pid", "1001".to_owned()),
        ("trade_no", format!("recovery-{order_no}")),
        ("out_trade_no", order_no.to_owned()),
        ("trade_status", "TRADE_SUCCESS".to_owned()),
        (
            "money",
            order["params"]["money"].as_str().unwrap().to_owned(),
        ),
    ]);
    let mut url = reqwest::Url::parse(&format!("http://{}/pay/callback/epay", env.addr)).unwrap();
    url.query_pairs_mut()
        .extend_pairs(&params)
        .append_pair("sign", &epay_sign(&params))
        .append_pair("sign_type", "MD5");
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let key = format!("bal:{{{}}}", env.user_id);
    redis
        .set::<(), _, _>(&key, "wrong-type", None, None, false)
        .await
        .unwrap();
    let first = client.get(url.clone()).send().await.unwrap();
    let first_status = first.status();
    let response = first.text().await.unwrap();
    redis.del::<(), _>(&key).await.unwrap();
    assert_eq!(first_status, 200);
    assert_eq!(response, "success");
    // A fresh worker must finish the already-paid order without waiting for
    // another provider callback or relying on the original HTTP task.
    let fresh =
        okapi_ledger::BalanceLedger::new(okapi_store::connect_redis(&redis_url).await.unwrap());
    okapi::worker::sweep_expired_reservations(&env.pg, &fresh, chrono::Utc::now())
        .await
        .unwrap();
    let recovered = fresh.balance(env.user_id).await.unwrap().as_micros();
    let repeat = client.get(url).send().await.unwrap();
    assert_eq!(repeat.status(), 200);
    assert_eq!(repeat.text().await.unwrap(), "success");
    assert_eq!(
        recovered, 5_000_000,
        "valid paid callback ({first_status}) must survive Redis failure"
    );
    assert_eq!(
        fresh.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
    let facts: (i64, i64) = sqlx::query_as("SELECT count(*),coalesce(sum(delta_micro),0)::bigint FROM billing_events WHERE user_id=$1 AND event_type='recharge'")
        .bind(env.user_id).fetch_one(&env.pg).await.unwrap();
    assert_eq!(facts, (1, 5_000_000));
    let after: Option<String> = redis.hget(&key, "avail").await.unwrap();
    assert_eq!(after.as_deref(), Some("5000000"));
}

#[path = "fund_fault.rs"]
mod fault;

#[tokio::test]
async fn failed_intent_rolls_back_paid_order_and_can_retry() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let order: Value = client
        .post(format!("http://{}/api/me/topup", env.addr))
        .bearer_auth(&env.user_token)
        .json(&json!({"amount_micro":5_000_000,"gateway":"epay"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let order_no = order["order_no"].as_str().unwrap();
    let params = BTreeMap::from([
        ("pid", "1001".to_owned()),
        ("trade_no", format!("atomic-{order_no}")),
        ("out_trade_no", order_no.to_owned()),
        ("trade_status", "TRADE_SUCCESS".to_owned()),
        (
            "money",
            order["params"]["money"].as_str().unwrap().to_owned(),
        ),
    ]);
    let mut url = reqwest::Url::parse(&format!("http://{}/pay/callback/epay", env.addr)).unwrap();
    url.query_pairs_mut()
        .extend_pairs(&params)
        .append_pair("sign", &epay_sign(&params))
        .append_pair("sign_type", "MD5");
    let rule = fault::reject(&env.pg, env.user_id).await;
    let failed = client.get(url.clone()).send().await;
    fault::restore(&env.pg, &rule).await;
    assert_eq!(failed.unwrap().status(), 500);
    let status: i16 = sqlx::query_scalar("SELECT status FROM recharge_orders WHERE order_no=$1")
        .bind(order_no)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(
        status, 0,
        "failure to persist recovery must not mark order paid"
    );
    assert_eq!(fault::pending_count(&env.pg, env.user_id).await, 0);
    let claimed:i64 = sqlx::query_scalar("SELECT count(*) FROM payment_receipts WHERE order_id=(SELECT id FROM recharge_orders WHERE order_no=$1)")
        .bind(order_no).fetch_one(&env.pg).await.unwrap();
    assert_eq!(
        claimed, 0,
        "failed financial intent must release the provider transaction claim"
    );
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM billing_events WHERE user_id=$1 AND event_type='recharge'",
    )
    .bind(env.user_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(events, 0);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        0
    );
    let retry = client.get(url).send().await.unwrap();
    assert_eq!(retry.status(), 200);
    assert_eq!(retry.text().await.unwrap(), "success");
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        5_000_000
    );
    assert_eq!(fault::pending_count(&env.pg, env.user_id).await, 1);
}
