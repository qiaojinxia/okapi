use super::*;

async fn database() -> String {
    let base = std::env::var("DATABASE_URL").unwrap();
    let root = okapi_store::connect_pg(&base).await.unwrap();
    let suffix = Uuid::new_v4();
    let name = format!("okapi_sub_lifecycle_{}", suffix.simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&root)
        .await
        .unwrap();
    root.close().await;
    let mut url = reqwest::Url::parse(&base).unwrap();
    url.set_path(&name);
    let pg = okapi_store::connect_pg(url.as_str()).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let random = u64::from_be_bytes(suffix.as_bytes()[..8].try_into().unwrap());
    let first = i64::try_from(random & ((1_u64 << 47) - 1)).unwrap() + 1_000_000_000_000;
    sqlx::query("SELECT setval('users_id_seq',$1,false)")
        .bind(first)
        .execute(&pg)
        .await
        .unwrap();
    pg.close().await;
    url.to_string()
}
async fn bed(database: &str) -> Bed {
    setup_at(database, &std::env::var("OKAPI_REDIS_URL").unwrap()).await
}
async fn active(b: &Bed) -> okapi_store::subscriptions::Subscription {
    okapi_store::subscriptions::active_for_user(&b.pg, b.user_id)
        .await
        .unwrap()
        .unwrap()
}
async fn due(b: &Bed, expiry: bool, seconds: i64) {
    sqlx::query("UPDATE user_subscriptions SET window_start=now()-interval '2 days',window_end=now()-$2*interval '1 second',expires_at=CASE WHEN $3 THEN now()-interval '1 second' ELSE expires_at END WHERE user_id=$1 AND status=1")
        .bind(b.user_id).bind(seconds).bind(expiry).execute(&b.pg).await.unwrap();
}
async fn wait_for_waiters(pg: &PgPool, count: i64) {
    for _ in 0..100 {
        let waiting:i64=sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory'")
            .fetch_one(pg).await.unwrap();
        if waiting >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {count} advisory lock waiters");
}

#[tokio::test]
async fn stale_expiry_scan_cannot_cancel_a_concurrent_http_renewal() {
    let b = bed(&database().await).await;
    let code = super::recovery::plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    due(&b, true, 1).await;
    let guard = okapi_ledger::holds::UserGuard::acquire(&b.pg, b.user_id)
        .await
        .unwrap();
    let request = reqwest::Client::new()
        .post(format!(
            "http://{}/admin/users/{}/subscription",
            b.console, b.user_id
        ))
        .bearer_auth(&b.admin_token)
        .json(&json!({"plan_code":code}));
    let renewal = tokio::spawn(async move { request.send().await.unwrap() });
    wait_for_waiters(&b.pg, 1).await;
    let pg = b.pg.clone();
    let ledger = b.ledger.clone();
    let now = Utc::now();
    let tick =
        tokio::spawn(
            async move { okapi_ledger::subscriptions::tick(&pg, &ledger, now, 100).await },
        );
    wait_for_waiters(&b.pg, 2).await;
    drop(guard);
    assert_eq!(renewal.await.unwrap().status(), 200);
    let report = tick.await.unwrap().unwrap();
    let current = okapi_store::subscriptions::active_for_user(&b.pg, b.user_id)
        .await
        .unwrap();
    assert!(
        current.is_some(),
        "a stale expiry scan cancelled the successful renewal"
    );
    assert_eq!(report.expired, 0);
    assert_eq!(report.rolled, 1);
    assert!(b.in_group().await);
    assert_eq!(b.sub().await.0, 2_000_000);
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn one_broken_due_user_cannot_starve_the_next_user_with_a_one_row_batch() {
    let db = database().await;
    let bad = bed(&db).await;
    let good = bed(&db).await;
    for b in [&bad, &good] {
        let code = super::recovery::plan(b).await;
        assert_eq!(b.admin_grant(&code).await.status(), 200);
    }
    due(&bad, false, 2).await;
    due(&good, false, 1).await;
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE billing_events ADD CONSTRAINT fail_window CHECK(user_id<>{} OR event_type<>'sub_reset') NOT VALID",bad.user_id)))
        .execute(&bad.pg).await.unwrap();
    let now = Utc::now();
    let first = okapi_ledger::subscriptions::tick(&bad.pg, &bad.ledger, now, 1).await;
    let second = okapi_ledger::subscriptions::tick(&bad.pg, &bad.ledger, now, 1).await;
    assert!(
        first.is_ok(),
        "one user's bad state aborted the maintenance batch"
    );
    assert!(second.is_ok());
    assert!(
        active(&good).await.window_end > now,
        "the failed oldest user monopolized every batch"
    );
    good.assert_zero_drift().await;
}

#[tokio::test]
async fn one_broken_accepted_user_cannot_starve_other_paid_entitlements() {
    let db = database().await;
    let bad = bed(&db).await;
    let good = bed(&db).await;
    for b in [&bad, &good] {
        let code = super::recovery::plan(b).await;
        super::recovery::break_sub(b).await;
        assert_eq!(b.admin_grant(&code).await.status(), 200);
    }
    super::recovery::restore_sub(&good).await;
    for _ in 0..2 {
        okapi_ledger::subscriptions::recover(&bad.pg, &bad.ledger, 1)
            .await
            .unwrap();
    }
    assert_eq!(
        good.sub().await.0,
        2_000_000,
        "an older damaged user blocked another accepted grant"
    );
    good.assert_zero_drift().await;
}

#[tokio::test]
async fn admin_cannot_publish_an_unfulfillable_subscription() {
    let b = bed(&database().await).await;
    for (field, value) in [
        ("grant_micro", json!(9_007_199_254_740_992_i64)),
        ("duration_days", json!(i32::MAX)),
    ] {
        let mut body = b.sub_plan(&format!("invalid-{field}"), 1_000_000, 1_000_000, false);
        body[field] = value;
        let response = b.upsert_plan(body).await;
        assert_eq!(response.status(), 400, "invalid {field} was published");
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plans")
        .fetch_one(&b.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

async fn checkout(b: &Bed, code: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "http://{}/api/me/subscriptions/checkout",
            b.console
        ))
        .bearer_auth(&b.token)
        .json(&json!({"plan_code":code,"gateway":"epay"}))
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn legacy_invalid_terms_are_not_advertised_or_charged() {
    let b = bed(&database().await).await;
    let code = super::recovery::plan(&b).await;
    sqlx::query("UPDATE plans SET duration_days=$2 WHERE plan_code=$1")
        .bind(&code)
        .bind(i32::MAX)
        .execute(&b.pg)
        .await
        .unwrap();
    let list: Value = reqwest::Client::new()
        .get(format!("http://{}/api/plans", b.console))
        .bearer_auth(&b.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let purchase = checkout(&b, &code).await;
    let orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recharge_orders")
        .fetch_one(&b.pg)
        .await
        .unwrap();
    assert_eq!(
        purchase.status(),
        400,
        "unfulfillable plan created a payable order"
    );
    assert_eq!(orders, 0);
    assert_eq!(list["data"][0]["purchasable"], false);
}

#[tokio::test]
async fn epay_valid_price_stays_exact_when_intermediate_exceeds_bigint() {
    let b = bed(&database().await).await;
    let code = "large-price";
    assert_eq!(
        b.upsert_plan(b.sub_plan(code, 1_000_000, 1_400_000_000_000_000, false))
            .await
            .status(),
        200
    );
    let response = checkout(&b, code).await;
    assert_eq!(response.status(), 200);
    let order: Value = response.json().await.unwrap();
    assert_eq!(order["params"]["money"], "9800000000.00");
}

#[tokio::test]
async fn invalid_exchange_rate_never_creates_a_payable_order() {
    let b = bed(&database().await).await;
    let code = super::recovery::plan(&b).await;
    for rate in [0, -7000] {
        sqlx::query("UPDATE settings SET value=jsonb_set(value,'{usd_to_cny_milli}',$1) WHERE key='payment_epay'")
            .bind(json!(rate)).execute(&b.pg).await.unwrap();
        assert_eq!(checkout(&b, &code).await.status(), 400);
    }
    let orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recharge_orders")
        .fetch_one(&b.pg)
        .await
        .unwrap();
    assert_eq!(orders, 0);
}

#[tokio::test]
async fn one_failed_grant_does_not_monopolize_pending_acceptances() {
    let db = database().await;
    let bad = bed(&db).await;
    let good = bed(&db).await;
    sqlx::query("ALTER TABLE billing_events ADD CONSTRAINT reject_grants CHECK(event_type<>'sub_grant') NOT VALID")
        .execute(&bad.pg).await.unwrap();
    for b in [&bad, &good] {
        let code = super::recovery::plan(b).await;
        assert_eq!(b.admin_grant(&code).await.status(), 200);
    }
    sqlx::query("ALTER TABLE billing_events DROP CONSTRAINT reject_grants")
        .execute(&bad.pg)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE billing_events ADD CONSTRAINT reject_bad CHECK(event_type<>'sub_grant' OR user_id<>{}) NOT VALID",bad.user_id)))
        .execute(&bad.pg).await.unwrap();
    for _ in 0..2 {
        okapi_ledger::subscriptions::recover(&bad.pg, &bad.ledger, 1)
            .await
            .unwrap();
    }
    assert_eq!(good.sub().await.0, 2_000_000);
    assert!(bad.mine().await["subscription"].is_null());
    sqlx::query("ALTER TABLE billing_events DROP CONSTRAINT reject_bad")
        .execute(&bad.pg)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE subscription_grants SET retry_after=now()-interval '1 second' WHERE user_id=$1",
    )
    .bind(bad.user_id)
    .execute(&bad.pg)
    .await
    .unwrap();
    for _ in 0..2 {
        okapi_ledger::subscriptions::recover(&bad.pg, &bad.ledger, 1)
            .await
            .unwrap();
    }
    assert_eq!(bad.sub().await.0, 2_000_000);
    let grants: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM billing_events WHERE user_id=$1 AND event_type='sub_grant'",
    )
    .bind(bad.user_id)
    .fetch_one(&bad.pg)
    .await
    .unwrap();
    assert_eq!(grants, 1);
    bad.assert_zero_drift().await;
    good.assert_zero_drift().await;
}

#[tokio::test]
async fn exchange_rounds_once_and_rejects_out_of_range_orders() {
    let b = bed(&database().await).await;
    let code = "currency-boundary";
    assert_eq!(
        b.upsert_plan(b.sub_plan(code, 1_000_000, 9_000_000_000_000_000, false))
            .await
            .status(),
        200
    );
    assert_eq!(checkout(&b, code).await.status(), 400);
    let orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recharge_orders")
        .fetch_one(&b.pg)
        .await
        .unwrap();
    assert_eq!(orders, 0);
    assert_eq!(
        b.upsert_plan(b.sub_plan(code, 1_000_000, 1, false))
            .await
            .status(),
        200
    );
    sqlx::query("UPDATE settings SET value=jsonb_set(value,'{usd_to_cny_milli}','1') WHERE key='payment_epay'")
        .execute(&b.pg).await.unwrap();
    let response = checkout(&b, code).await;
    assert_eq!(response.status(), 200);
    let order: Value = response.json().await.unwrap();
    assert_eq!(order["params"]["money"], "0.01");
}

#[tokio::test]
async fn concurrent_maintenance_counts_and_funds_each_window_once() {
    let b = bed(&database().await).await;
    let code = super::recovery::plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    due(&b, false, 1).await;
    let guard = okapi_ledger::holds::UserGuard::acquire(&b.pg, b.user_id)
        .await
        .unwrap();
    let now = Utc::now();
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let pg = b.pg.clone();
        let ledger = b.ledger.clone();
        tasks.push(tokio::spawn(async move {
            okapi_ledger::subscriptions::tick(&pg, &ledger, now, 100).await
        }));
    }
    wait_for_waiters(&b.pg, 2).await;
    drop(guard);
    let mut rolled = 0;
    for task in tasks {
        rolled += task.await.unwrap().unwrap().rolled;
    }
    assert_eq!(rolled, 1);
    let resets: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM billing_events WHERE user_id=$1 AND event_type='sub_reset'",
    )
    .bind(b.user_id)
    .fetch_one(&b.pg)
    .await
    .unwrap();
    assert_eq!(resets, 1);
    b.assert_zero_drift().await;
}
