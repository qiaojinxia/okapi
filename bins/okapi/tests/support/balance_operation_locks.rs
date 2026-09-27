use super::*;
use okapi_ledger::holds::UserGuard;

// Observe the competing request actually waiting for this user's PG lock. A
// sleep alone can pass accidentally when the HTTP handler has not started yet.
async fn waiting_for_user_lock(
    env: &Env,
    request: &mut tokio::task::JoinHandle<reqwest::Response>,
) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            tokio::select! {
                result = &mut *request => panic!("money operation bypassed recovery lock: {:?}", result.unwrap().status()),
                waiting = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid=$1::bigint::oid AND objid=(hashtext($2)::bigint & 4294967295)::oid)"
                ).bind(okapi_store::image_batches::HOLD_LOCK_NAMESPACE).bind(env.user_id.to_string()).fetch_one(&env.pg) => {
                    if waiting.unwrap() { return; }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}

async fn check_operation_waits(refund: bool) {
    let env = setup().await;
    let (path, body, before, after) = if refund {
        let (request_id, amount) = chat_settled(&env).await;
        (
            "/admin/billing/refund".to_owned(),
            json!({"request_id": request_id, "reason": "recovery race"}),
            1_000_000 - amount,
            1_000_000,
        )
    } else {
        (
            format!("/admin/users/{}/credit", env.user_id),
            json!({"amount_micro": 500, "reason": "recovery race"}),
            1_000_000,
            1_000_500,
        )
    };
    let mut guard = UserGuard::acquire(&env.pg, env.user_id).await.unwrap();
    let send = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(&env.super_token)
        .json(&body);
    let mut request = tokio::spawn(async move { send.send().await.unwrap() });
    waiting_for_user_lock(&env, &mut request).await;
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        before
    );
    let snapshot: i64 = sqlx::query_scalar("SELECT balance_micro FROM users WHERE id=$1")
        .bind(env.user_id)
        .fetch_one(guard.connection())
        .await
        .unwrap();
    assert_eq!(
        snapshot, before,
        "PG must not move ahead of the recovery lock"
    );
    guard
        .repair(&env.state.ledger, Money::from_micros(before), Money::ZERO)
        .await
        .unwrap();
    drop(guard);
    let response = tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        after
    );
    let snapshot: i64 = sqlx::query_scalar("SELECT balance_micro FROM users WHERE id=$1")
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(snapshot, after);
}

#[tokio::test]
async fn credit_waits_for_balance_recovery() {
    check_operation_waits(false).await;
}

#[tokio::test]
async fn refund_waits_for_balance_recovery() {
    check_operation_waits(true).await;
}

#[tokio::test]
async fn credit_unknown_user_is_not_found_without_orphan_money() {
    let env = setup().await;
    let absent = -env.user_id;
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/admin/users/{absent}/credit",
            env.console
        ))
        .bearer_auth(&env.super_token)
        .json(&json!({"amount_micro": 500, "reason": "unknown user"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(env.state.ledger.balance(absent).await.unwrap(), Money::ZERO);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_events WHERE user_id=$1")
        .bind(absent)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
