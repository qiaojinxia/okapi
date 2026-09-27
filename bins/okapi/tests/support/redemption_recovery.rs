use super::*;
use fred::interfaces::{HashesInterface, KeysInterface};

#[tokio::test]
async fn claimed_code_survives_redis_failure_without_becoming_free_or_lost() {
    let env = setup().await;
    let code = format!("recover-{}", Uuid::new_v4());
    okapi_store::admin::create_redemption_codes(
        &env.pg,
        env.user_id,
        3_000_000,
        std::slice::from_ref(&code),
        None,
        okapi_store::admin::RedemptionOptions {
            bind_user_id: Some(env.user_id),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let key = format!("bal:{{{}}}", env.user_id);
    redis
        .set::<(), _, _>(&key, "wrong-type", None, None, false)
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": code}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let receipt: Value = response.json().await.unwrap();
    redis.del::<(), _>(&key).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(receipt["pending"], true);
    assert!(receipt["balance_after_micro"].is_null());
    assert!(receipt["operation_id"].as_str().is_some());
    let fresh =
        okapi_ledger::BalanceLedger::new(okapi_store::connect_redis(&redis_url).await.unwrap());
    okapi::worker::sweep_expired_reservations(&env.pg, &fresh, chrono::Utc::now())
        .await
        .unwrap();
    let after = fresh.balance(env.user_id).await.unwrap().as_micros();
    let replay = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": code}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        replay.status(),
        404,
        "a used code must not grant funds again"
    );
    assert_eq!(
        after, 3_000_000,
        "claimed code ({status}) must retain a durable credit"
    );
    assert_eq!(fresh.balance(env.user_id).await.unwrap().as_micros(), after);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM billing_events WHERE user_id=$1 AND actor='system:redeem'",
    )
    .bind(env.user_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let value: Option<String> = redis.hget(&key, "avail").await.unwrap();
    assert_eq!(value.as_deref(), Some("3000000"));
}

#[path = "fund_fault.rs"]
mod fault;

#[tokio::test]
async fn failed_intent_rolls_back_code_claim_and_can_retry() {
    let env = setup().await;
    let code = format!("atomic-{}", Uuid::new_v4());
    okapi_store::admin::create_redemption_codes(
        &env.pg,
        env.user_id,
        3_000_000,
        std::slice::from_ref(&code),
        None,
        okapi_store::admin::RedemptionOptions {
            bind_user_id: Some(env.user_id),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let request = || {
        reqwest::Client::new()
            .post(format!("http://{}/api/me/redeem", env.addr))
            .bearer_auth(&env.user_token)
            .header("x-real-ip", uniq_ip())
            .json(&json!({"code":code}))
    };
    let rule = fault::reject(&env.pg, env.user_id).await;
    let failed = request().send().await;
    fault::restore(&env.pg, &rule).await;
    assert_eq!(failed.unwrap().status(), 500);
    assert_eq!(fault::pending_count(&env.pg, env.user_id).await, 0);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM billing_events WHERE user_id=$1 AND actor='system:redeem'",
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
    let retry = request().send().await.unwrap();
    assert_eq!(
        retry.status(),
        200,
        "failed PG commit must leave code usable"
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        3_000_000
    );
    assert_eq!(fault::pending_count(&env.pg, env.user_id).await, 1);
}
