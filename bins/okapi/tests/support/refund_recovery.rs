use super::*;
use fred::interfaces::HashesInterface;

#[tokio::test]
async fn committed_refund_survives_redis_failure_and_replay() {
    let env = setup().await;
    let (request_id, _) = chat_settled(&env).await;
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let key = format!("bal:{{{}}}", env.user_id);
    redis
        .hset::<(), _, _>(&key, ("avail", "invalid"))
        .await
        .unwrap();
    let request = || {
        reqwest::Client::new()
            .post(format!("http://{}/admin/billing/refund", env.console))
            .bearer_auth(&env.super_token)
            .json(&json!({"request_id":request_id,"reason":"failure recovery"}))
    };
    let first = request().send().await.unwrap();
    let status = first.status();
    let receipt: Value = first.json().await.unwrap();
    redis
        .hset::<(), _, _>(&key, ("avail", before.as_micros()))
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(receipt["pending"], true);
    assert!(receipt["balance_after_micro"].is_null());
    assert!(receipt["operation_id"].as_str().is_some());
    let fresh =
        okapi_ledger::BalanceLedger::new(okapi_store::connect_redis(&redis_url).await.unwrap());
    okapi::worker::sweep_expired_reservations(&env.pg, &fresh, chrono::Utc::now())
        .await
        .unwrap();
    let recovered = fresh.balance(env.user_id).await.unwrap().as_micros();
    let replay = request().send().await.unwrap();
    assert_eq!(replay.status(), 200);
    assert_eq!(
        replay.json::<Value>().await.unwrap()["outcome"],
        "already_refunded"
    );
    assert_eq!(
        recovered, 1_000_000,
        "committed refund ({status}) must recover without another refund"
    );
    assert_eq!(
        fresh.balance(env.user_id).await.unwrap().as_micros(),
        recovered
    );
    let facts: (i64,i64) = sqlx::query_as("SELECT count(*),sum(delta_micro)::bigint FROM billing_events WHERE request_id=$1 AND event_type='refund'")
        .bind(request_id).fetch_one(&env.pg).await.unwrap();
    assert_eq!(facts, (1, 240));
}

#[path = "fund_fault.rs"]
mod fault;

#[tokio::test]
async fn failed_intent_rolls_back_refund_and_can_retry() {
    let env = setup().await;
    let (request_id, _) = chat_settled(&env).await;
    let before = env.state.ledger.balance(env.user_id).await.unwrap();
    let request = || {
        reqwest::Client::new()
            .post(format!("http://{}/admin/billing/refund", env.console))
            .bearer_auth(&env.super_token)
            .json(&json!({"request_id":request_id,"reason":"atomic refund"}))
    };
    let rule = fault::reject(&env.pg, env.user_id).await;
    let failed = request().send().await;
    fault::restore(&env.pg, &rule).await;
    assert_eq!(failed.unwrap().status(), 500);
    let status: i16 = sqlx::query_scalar("SELECT status FROM billing_records WHERE request_id=$1")
        .bind(request_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 20);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM billing_events WHERE request_id=$1 AND event_type='refund'",
    )
    .bind(request_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(events, 0);
    assert_eq!(fault::pending_count(&env.pg, env.user_id).await, 0);
    assert_eq!(env.state.ledger.balance(env.user_id).await.unwrap(), before);
    let retry = request().send().await.unwrap();
    assert_eq!(retry.status(), 200);
    assert_eq!(retry.json::<Value>().await.unwrap()["outcome"], "refunded");
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        1_000_000
    );
    assert_eq!(fault::pending_count(&env.pg, env.user_id).await, 1);
}
