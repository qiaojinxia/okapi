use super::*;
use fred::interfaces::HashesInterface;

pub(super) async fn plan(b: &Bed) -> String {
    let code = format!("recover-{}", b.suffix);
    assert_eq!(
        b.upsert_plan(b.sub_plan(&code, 2_000_000, 9_990_000, true))
            .await
            .status(),
        200
    );
    code
}
async fn callback(b: &Bed, code: &str) -> reqwest::Url {
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/api/me/subscriptions/checkout",
            b.console
        ))
        .bearer_auth(&b.token)
        .json(&json!({"plan_code":code,"gateway":"epay"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let order: Value = response.json().await.unwrap();
    let no = order["order_no"].as_str().unwrap();
    let fields = BTreeMap::from([
        ("pid", "1001".to_owned()),
        ("trade_no", format!("sub-{no}")),
        ("out_trade_no", no.to_owned()),
        ("trade_status", "TRADE_SUCCESS".to_owned()),
        (
            "money",
            order["params"]["money"].as_str().unwrap().to_owned(),
        ),
    ]);
    let mut url = reqwest::Url::parse(&format!("http://{}/pay/callback/epay", b.console)).unwrap();
    url.query_pairs_mut()
        .extend_pairs(&fields)
        .append_pair("sign", &epay_sign(&fields))
        .append_pair("sign_type", "MD5");
    url
}
pub(super) async fn break_sub(b: &Bed) {
    b.redis
        .hset::<(), _, _>(format!("bal:{{{}}}", b.user_id), ("sub", "invalid"))
        .await
        .unwrap();
}
pub(super) async fn restore_sub(b: &Bed) {
    b.redis
        .hset::<(), _, _>(format!("bal:{{{}}}", b.user_id), ("sub", "0"))
        .await
        .unwrap();
}
async fn fresh_worker(b: &Bed) {
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let ledger = BalanceLedger::new(redis.clone());
    worker::sweep_expired_reservations(&b.pg, &ledger, Utc::now())
        .await
        .unwrap();
    worker::subscriptions_tick(&b.pg, &ledger, &redis, Utc::now())
        .await
        .unwrap();
}
async fn new_group(b: &Bed) -> String {
    let code = format!("other-{}", b.suffix);
    okapi_store::admin::upsert_price_group(
        &b.pg,
        okapi_store::admin::PriceGroupInput {
            group_code: &code,
            group_ratio: "1.0",
            description: "",
            pool_code: None,
            self_select: false,
            rpm_limit: None,
            rph_limit: None,
        },
    )
    .await
    .unwrap();
    code
}

#[tokio::test]
async fn paid_subscription_recovers_after_redis_failure_without_another_callback() {
    let b = setup().await;
    let code = plan(&b).await;
    let url = callback(&b, &code).await;
    break_sub(&b).await;
    let response = reqwest::Client::new()
        .get(url.clone())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    restore_sub(&b).await;
    assert_eq!(status, 200);
    assert_eq!(body, "success");
    fresh_worker(&b).await;
    assert_eq!(
        b.sub().await.0,
        2_000_000,
        "a paid subscription must be delivered by recovery"
    );
    let before = b.mine().await["subscription"]["expires_at"].clone();
    assert_eq!(
        reqwest::Client::new()
            .get(url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "success"
    );
    assert_eq!(b.mine().await["subscription"]["expires_at"], before);
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn consumed_subscription_code_recovers_and_returns_an_accepted_receipt() {
    let b = setup().await;
    let code = plan(&b).await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/admin/redemptions", b.console))
        .bearer_auth(&b.admin_token)
        .json(&json!({"count":1,"amount_micro":1,"plan_code":code}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let created: Value = response.json().await.unwrap();
    break_sub(&b).await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/me/redeem", b.console))
        .bearer_auth(&b.token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code":created["codes"][0]}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    restore_sub(&b).await;
    fresh_worker(&b).await;
    assert_eq!(
        b.sub().await.0,
        2_000_000,
        "a consumed code must not permanently lose its subscription"
    );
    assert_eq!(status, 200, "{body}");
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn same_admin_idempotency_key_does_not_renew_twice() {
    let b = setup().await;
    let code = plan(&b).await;
    let id = Uuid::new_v4().to_string();
    let request = || {
        reqwest::Client::new()
            .post(format!(
                "http://{}/admin/users/{}/subscription",
                b.console, b.user_id
            ))
            .bearer_auth(&b.admin_token)
            .header("Idempotency-Key", &id)
            .json(&json!({"plan_code":code}))
    };
    let first = request().send().await.unwrap();
    assert_eq!(first.status(), 200);
    let before = b.mine().await["subscription"]["expires_at"].clone();
    let second = request().send().await.unwrap();
    assert_eq!(second.status(), 200);
    assert_eq!(
        b.mine().await["subscription"]["expires_at"],
        before,
        "a retry is not another free renewal"
    );
    // Distinct operations remain legitimate renewals.
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    assert_ne!(b.mine().await["subscription"]["expires_at"], before);
}

#[tokio::test]
async fn payment_delivers_the_purchased_plan_snapshot_after_catalog_edit() {
    let b = setup().await;
    let code = plan(&b).await;
    let url = callback(&b, &code).await;
    let other = new_group(&b).await;
    let mut changed = b.sub_plan(&code, 7_000_000, 1_000_000, false);
    changed["group_code"] = json!(other);
    changed["period"] = json!(2);
    changed["duration_days"] = json!(90);
    assert_eq!(b.upsert_plan(changed).await.status(), 200);
    let result = reqwest::Client::new().get(url).send().await.unwrap();
    assert_eq!(result.text().await.unwrap(), "success");
    let mine = b.mine().await;
    let sub = &mine["subscription"];
    assert_eq!(
        sub["quota_micro"], 2_000_000,
        "paid quota must not follow a later catalog edit"
    );
    assert_eq!(sub["period"], 1);
    assert_eq!(sub["group_code"], b.group);
    let starts: chrono::DateTime<Utc> = serde_json::from_value(sub["starts_at"].clone()).unwrap();
    let expires: chrono::DateTime<Utc> = serde_json::from_value(sub["expires_at"].clone()).unwrap();
    assert_eq!(expires - starts, ChronoDuration::days(30));
}

#[tokio::test]
async fn active_subscription_period_and_group_do_not_follow_catalog_edits() {
    let b = setup().await;
    let code = plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    let other = new_group(&b).await;
    let mut changed = b.sub_plan(&code, 7_000_000, 1_000_000, false);
    changed["group_code"] = json!(other);
    changed["period"] = json!(2);
    assert_eq!(b.upsert_plan(changed).await.status(), 200);
    let mine = b.mine().await;
    assert_eq!(mine["subscription"]["period"], 1);
    assert_eq!(mine["subscription"]["group_code"], b.group);
}

#[tokio::test]
async fn cancellation_revokes_only_the_group_originally_granted() {
    let b = setup().await;
    let code = plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    let other = new_group(&b).await;
    sqlx::query("INSERT INTO user_groups(user_id,group_code) VALUES($1,$2)")
        .bind(b.user_id)
        .bind(&other)
        .execute(&b.pg)
        .await
        .unwrap();
    let mut changed = b.sub_plan(&code, 2_000_000, 9_990_000, false);
    changed["group_code"] = json!(other);
    assert_eq!(b.upsert_plan(changed).await.status(), 200);
    let response = reqwest::Client::new()
        .delete(format!(
            "http://{}/admin/users/{}/subscription",
            b.console, b.user_id
        ))
        .bearer_auth(&b.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let other_kept: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_groups WHERE user_id=$1 AND group_code=$2)",
    )
    .bind(b.user_id)
    .bind(other)
    .fetch_one(&b.pg)
    .await
    .unwrap();
    assert!(
        other_kept,
        "cancel must not revoke an unrelated group after a plan edit"
    );
    assert!(
        !b.in_group().await,
        "original granted group must be revoked"
    );
}

#[tokio::test]
async fn grant_event_failure_recovers_without_unrecorded_subscription_money() {
    let b = setup().await;
    let code = plan(&b).await;
    let rule = format!("sub_fault_{}", b.suffix);
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE billing_events ADD CONSTRAINT {rule} CHECK (user_id<>{} OR event_type<>'sub_grant') NOT VALID",b.user_id)))
        .execute(&b.pg).await.unwrap();
    let response = b.admin_grant(&code).await;
    let status = response.status();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_events DROP CONSTRAINT {rule}"
    )))
    .execute(&b.pg)
    .await
    .unwrap();
    fresh_worker(&b).await;
    let total:i64=sqlx::query_scalar("SELECT COALESCE(SUM(delta_micro),0)::bigint FROM billing_events WHERE user_id=$1 AND pool=1")
        .bind(b.user_id).fetch_one(&b.pg).await.unwrap();
    assert_eq!(
        total, 2_000_000,
        "accepted subscription must durably recover its grant event (HTTP {status})"
    );
    assert_eq!(b.sub().await.0, total);
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn ended_grant_source_cannot_activate_a_new_subscription_on_replay() {
    let b = setup().await;
    let code = plan(&b).await;
    let p = okapi_store::subscriptions::find_sub_plan(&b.pg, &code)
        .await
        .unwrap()
        .unwrap();
    let source = format!("purchase:fixed-{}", b.suffix);
    let first =
        okapi_ledger::subscriptions::grant(&b.pg, &b.ledger, b.user_id, &p, &source, "test")
            .await
            .unwrap();
    let id = first.subscription().id;
    okapi_ledger::subscriptions::end(&b.pg, &b.ledger, id, 3, "test")
        .await
        .unwrap();
    let replay =
        okapi_ledger::subscriptions::grant(&b.pg, &b.ledger, b.user_id, &p, &source, "test")
            .await
            .unwrap();
    assert_eq!(
        replay.subscription().id,
        id,
        "a historical source must not issue a second subscription"
    );
    assert!(
        okapi_store::subscriptions::active_for_user(&b.pg, b.user_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(b.sub().await.0, 0);
}

#[tokio::test]
async fn pending_subscription_view_does_not_claim_spendable_credit() {
    let b = setup().await;
    let code = plan(&b).await;
    let url = callback(&b, &code).await;
    break_sub(&b).await;
    let response = reqwest::Client::new().get(url).send().await.unwrap();
    let mine = b.mine().await;
    restore_sub(&b).await;
    assert_eq!(response.text().await.unwrap(), "success");
    assert_eq!(mine["subscription"]["pending"], true);
    assert!(mine["subscription"]["remaining_micro"].is_null());
    fresh_worker(&b).await;
    assert_eq!(b.mine().await["subscription"]["remaining_micro"], 2_000_000);
}

#[tokio::test]
async fn issued_subscription_code_keeps_terms_when_plan_is_disabled() {
    let b = setup().await;
    let code = plan(&b).await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/admin/redemptions", b.console))
        .bearer_auth(&b.admin_token)
        .json(&json!({"count":1,"amount_micro":7,"plan_code":code}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let created: Value = response.json().await.unwrap();
    sqlx::query("UPDATE plans SET status=2,grant_micro=9000000,period=3 WHERE plan_code=$1")
        .bind(&code)
        .execute(&b.pg)
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/me/redeem", b.console))
        .bearer_auth(&b.token)
        .json(&json!({"code":created["codes"][0]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(b.sub().await.0, 2_000_000);
    assert_eq!(b.mine().await["subscription"]["period"], 1);
    assert_eq!(b.wallet().await, WALLET);
}

#[tokio::test]
async fn paid_conflicting_plan_stays_visible_and_delivers_after_active_plan_ends() {
    let b = setup().await;
    let code = plan(&b).await;
    let url = callback(&b, &code).await;
    let other = format!("blocking-{}", b.suffix);
    assert_eq!(
        b.upsert_plan(b.sub_plan(&other, 1_000_000, 0, false))
            .await
            .status(),
        200
    );
    assert_eq!(b.admin_grant(&other).await.status(), 200);
    let response = reqwest::Client::new().get(url).send().await.unwrap();
    assert_eq!(response.text().await.unwrap(), "success");
    let mine = b.mine().await;
    assert_eq!(mine["subscription"]["plan_code"], other);
    assert_eq!(mine["pending_grants"][0]["plan_code"], code);
    assert_eq!(mine["pending_grants"][0]["reason"], "subscription_active");
    let cancel = reqwest::Client::new()
        .delete(format!(
            "http://{}/admin/users/{}/subscription",
            b.console, b.user_id
        ))
        .bearer_auth(&b.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), 200);
    fresh_worker(&b).await;
    let mine = b.mine().await;
    assert_eq!(mine["subscription"]["plan_code"], code);
    assert!(mine["pending_grants"].as_array().unwrap().is_empty());
    assert_eq!(b.sub().await.0, 2_000_000);
}

#[tokio::test]
async fn concurrent_admin_retries_accept_one_grant_and_one_window() {
    let b = setup().await;
    let code = plan(&b).await;
    let id = Uuid::new_v4().to_string();
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let request = reqwest::Client::new()
            .post(format!(
                "http://{}/admin/users/{}/subscription",
                b.console, b.user_id
            ))
            .bearer_auth(&b.admin_token)
            .header("Idempotency-Key", &id)
            .json(&json!({"plan_code":code}));
        jobs.spawn(async move { request.send().await.unwrap() });
    }
    let mut ids = std::collections::HashSet::new();
    while let Some(response) = jobs.join_next().await {
        let response = response.unwrap();
        assert_eq!(response.status(), 200);
        ids.insert(
            response.json::<Value>().await.unwrap()["operation_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(ids.len(), 1);
    let duration:i64=sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM expires_at-starts_at)::bigint FROM user_subscriptions WHERE user_id=$1 AND status=1")
        .bind(b.user_id).fetch_one(&b.pg).await.unwrap();
    assert_eq!(duration, 30 * 86400);
    let grant_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM billing_events WHERE user_id=$1 AND event_type='sub_grant'",
    )
    .bind(b.user_id)
    .fetch_one(&b.pg)
    .await
    .unwrap();
    assert_eq!(grant_count, 1);
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn accepted_admin_key_survives_catalog_disable_and_rejects_another_plan() {
    let b = setup().await;
    let code = plan(&b).await;
    let id = Uuid::new_v4().to_string();
    let request = |plan: &str| {
        reqwest::Client::new()
            .post(format!(
                "http://{}/admin/users/{}/subscription",
                b.console, b.user_id
            ))
            .bearer_auth(&b.admin_token)
            .header("Idempotency-Key", &id)
            .json(&json!({"plan_code":plan}))
    };
    let first = request(&code).send().await.unwrap();
    assert_eq!(first.status(), 200);
    let first: Value = first.json().await.unwrap();
    sqlx::query("UPDATE plans SET status=2 WHERE plan_code=$1")
        .bind(&code)
        .execute(&b.pg)
        .await
        .unwrap();
    let again = request(&code).send().await.unwrap();
    assert_eq!(again.status(), 200);
    assert_eq!(
        again.json::<Value>().await.unwrap()["operation_id"],
        first["operation_id"]
    );
    let other = format!("other-{}", b.suffix);
    assert_eq!(
        b.upsert_plan(b.sub_plan(&other, 1_000_000, 0, false))
            .await
            .status(),
        200
    );
    assert_eq!(request(&other).send().await.unwrap().status(), 409);
    assert_eq!(cancel(&b).await.status(), 200);
    assert_eq!(b.admin_grant(&other).await.status(), 200);
    let ended: Value = request(&code).send().await.unwrap().json().await.unwrap();
    assert_eq!(ended["operation_id"], first["operation_id"]);
    assert_eq!(ended["subscription"]["status"], 3);
    assert_eq!(ended["subscription"]["remaining_micro"], 0);
    assert_eq!(b.mine().await["subscription"]["plan_code"], other);
    assert_eq!(b.sub().await.0, 1_000_000);
}

async fn reject_acceptance(b: &Bed) -> String {
    let name = format!("reject_sub_{}", b.suffix);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE subscription_grants ADD CONSTRAINT {name} CHECK(user_id<>{}) NOT VALID",
        b.user_id
    )))
    .execute(&b.pg)
    .await
    .unwrap();
    name
}
async fn restore_acceptance(b: &Bed, name: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE subscription_grants DROP CONSTRAINT {name}"
    )))
    .execute(&b.pg)
    .await
    .unwrap();
}
#[tokio::test]
async fn failed_grant_acceptance_cannot_mark_the_subscription_order_paid() {
    let b = setup().await;
    let code = plan(&b).await;
    let url = callback(&b, &code).await;
    let name = reject_acceptance(&b).await;
    let response = reqwest::Client::new().get(url.clone()).send().await;
    restore_acceptance(&b, &name).await;
    assert_eq!(response.unwrap().status(), 500);
    let status: i16 = sqlx::query_scalar("SELECT status FROM recharge_orders WHERE user_id=$1")
        .bind(b.user_id)
        .fetch_one(&b.pg)
        .await
        .unwrap();
    assert_eq!(status, 0);
    let retry = reqwest::Client::new().get(url).send().await.unwrap();
    assert_eq!(retry.text().await.unwrap(), "success");
    assert_eq!(b.sub().await.0, 2_000_000);
}
#[tokio::test]
async fn failed_grant_acceptance_cannot_consume_the_subscription_code() {
    let b = setup().await;
    let code = plan(&b).await;
    let created: Value = reqwest::Client::new()
        .post(format!("http://{}/admin/redemptions", b.console))
        .bearer_auth(&b.admin_token)
        .json(&json!({"count":1,"amount_micro":1,"plan_code":code}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request = || {
        reqwest::Client::new()
            .post(format!("http://{}/api/me/redeem", b.console))
            .bearer_auth(&b.token)
            .json(&json!({"code":created["codes"][0]}))
    };
    let name = reject_acceptance(&b).await;
    let response = request().send().await;
    restore_acceptance(&b, &name).await;
    assert_eq!(response.unwrap().status(), 500);
    let retry = request().send().await.unwrap();
    assert_eq!(retry.status(), 200);
    assert_eq!(b.sub().await.0, 2_000_000);
}

#[tokio::test]
async fn stale_window_tick_does_not_refill_consumed_credit() {
    let b = setup().await;
    let code = plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    // Stay away from the exact rollover boundary: PG and gateway clocks can differ slightly.
    sqlx::query("UPDATE user_subscriptions SET window_start=now()-interval '49 hours',window_end=now()-interval '25 hours' WHERE user_id=$1 AND status=1")
        .bind(b.user_id).execute(&b.pg).await.unwrap();
    let old = okapi_store::subscriptions::active_for_user(&b.pg, b.user_id)
        .await
        .unwrap()
        .unwrap();
    let now = Utc::now();
    okapi_ledger::subscriptions::roll(&b.pg, &b.ledger, &old, now, "test")
        .await
        .unwrap();
    assert_eq!(b.chat().await, 200);
    let (_, charged, pool) = b.wait_committed(&[]).await;
    assert!(charged > 0);
    assert_eq!(
        pool, 1,
        "the request must actually consume the rolled subscription"
    );
    let spent = b.sub().await.0;
    assert!(spent < 2_000_000);
    okapi_ledger::subscriptions::roll(&b.pg, &b.ledger, &old, now, "test")
        .await
        .unwrap();
    assert_eq!(
        b.sub().await.0,
        spent,
        "a stale tick is not a second quota grant"
    );
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn paid_renewal_with_changed_terms_is_not_silently_fulfilled_with_old_quota() {
    let b = setup().await;
    let code = plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    // The renewal quote is made while its original terms are still compatible.
    let url = callback(&b, &code).await;
    // Replace that active instance after quote, with the same plan id but new terms.
    let cancel_url = format!(
        "http://{}/admin/users/{}/subscription",
        b.console, b.user_id
    );
    assert_eq!(
        reqwest::Client::new()
            .delete(&cancel_url)
            .bearer_auth(&b.admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        b.upsert_plan(b.sub_plan(&code, 7_000_000, 9_990_000, true))
            .await
            .status(),
        200
    );
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    let response = reqwest::Client::new().get(url).send().await.unwrap();
    assert_eq!(response.text().await.unwrap(), "success");
    let mine = b.mine().await;
    assert_eq!(mine["subscription"]["quota_micro"], 7_000_000);
    assert_eq!(mine["pending_grants"][0]["reason"], "subscription_active");
    assert_eq!(
        reqwest::Client::new()
            .delete(&cancel_url)
            .bearer_auth(&b.admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    fresh_worker(&b).await;
    assert_eq!(b.mine().await["subscription"]["quota_micro"], 2_000_000);
    assert_eq!(b.sub().await.0, 2_000_000);
}

async fn cancel(b: &Bed) -> reqwest::Response {
    reqwest::Client::new()
        .delete(format!(
            "http://{}/admin/users/{}/subscription",
            b.console, b.user_id
        ))
        .bearer_auth(&b.admin_token)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn pending_grants_are_paginated_and_scoped_to_the_authenticated_user() {
    let b = setup().await;
    let code = plan(&b).await;
    let plan = okapi_store::subscriptions::find_sub_plan(&b.pg, &code)
        .await
        .unwrap()
        .unwrap();
    let other = okapi_store::provision::create_user(&b.pg, &format!("queue-other-{}", b.suffix))
        .await
        .unwrap();
    let mut tx = b.pg.begin().await.unwrap();
    let mut ids = Vec::new();
    for i in 0..25 {
        ids.push(
            okapi_ledger::subscriptions::enqueue(
                &mut tx,
                b.user_id,
                &plan,
                &format!("page:{i}"),
                "test",
                true,
            )
            .await
            .unwrap()
            .to_string(),
        );
    }
    let other_id =
        okapi_ledger::subscriptions::enqueue(&mut tx, other, &plan, "other:page", "test", true)
            .await
            .unwrap()
            .to_string();
    tx.commit().await.unwrap();
    let first = b.mine().await;
    let page = first["pending_grants"].as_array().unwrap();
    assert_eq!(page.len(), 20);
    assert_eq!(page[0]["operation_id"], ids[24]);
    let cursor = first["pending_next_before"].as_i64().unwrap();
    let response = reqwest::Client::new()
        .get(format!(
            "http://{}/api/me/subscription?pending_before={cursor}",
            b.console
        ))
        .bearer_auth(&b.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let second: Value = response.json().await.unwrap();
    assert_eq!(second["pending_grants"].as_array().unwrap().len(), 5);
    assert!(second["pending_next_before"].is_null());
    let actual: Vec<_> = page
        .iter()
        .chain(second["pending_grants"].as_array().unwrap())
        .map(|v| v["operation_id"].as_str().unwrap().to_owned())
        .collect();
    ids.reverse();
    assert_eq!(actual, ids);
    assert!(!actual.contains(&other_id));
    for cursor in ["0", "-1", "invalid"] {
        let response = reqwest::Client::new()
            .get(format!(
                "http://{}/api/me/subscription?pending_before={cursor}",
                b.console
            ))
            .bearer_auth(&b.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let response: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/users/{}/subscription",
            b.console, b.user_id
        ))
        .bearer_auth(&b.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["pending_grants"], first["pending_grants"]);
}

async fn reject_event(b: &Bed, event: &str) -> String {
    let name = format!("event_fault_{}", b.suffix);
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE billing_events ADD CONSTRAINT {name} CHECK(user_id<>{} OR event_type<>'{event}') NOT VALID",b.user_id)))
        .execute(&b.pg).await.unwrap();
    name
}
async fn restore_event(b: &Bed, name: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_events DROP CONSTRAINT {name}"
    )))
    .execute(&b.pg)
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_cancellation_event_keeps_subscription_group_and_credit() {
    let b = setup().await;
    let code = plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    let name = reject_event(&b, "sub_expire").await;
    let response = cancel(&b).await;
    restore_event(&b, &name).await;
    assert_eq!(response.status(), 500);
    assert_eq!(b.mine().await["subscription"]["status"], 1);
    assert!(b.in_group().await);
    assert_eq!(b.sub().await.0, 2_000_000);
    assert_eq!(cancel(&b).await.status(), 200);
    assert!(b.mine().await["subscription"].is_null());
    assert!(!b.in_group().await);
    assert_eq!(b.sub().await.0, 0);
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn failed_window_event_cannot_advance_the_period_or_refill_credit() {
    let b = setup().await;
    let code = plan(&b).await;
    assert_eq!(b.admin_grant(&code).await.status(), 200);
    assert_eq!(b.chat().await, 200);
    b.wait_committed(&[]).await;
    let before = b.sub().await;
    let sub = okapi_store::subscriptions::active_for_user(&b.pg, b.user_id)
        .await
        .unwrap()
        .unwrap();
    let later = sub.window_end + ChronoDuration::seconds(1);
    let name = reject_event(&b, "sub_reset").await;
    let result = okapi_ledger::subscriptions::roll(&b.pg, &b.ledger, &sub, later, "test").await;
    restore_event(&b, &name).await;
    assert!(result.is_err());
    let current = okapi_store::subscriptions::active_for_user(&b.pg, b.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.window_start, sub.window_start);
    assert_eq!(current.window_end, sub.window_end);
    assert_eq!(b.sub().await, before);
    okapi_ledger::subscriptions::roll(&b.pg, &b.ledger, &sub, later, "test")
        .await
        .unwrap();
    assert_eq!(b.sub().await.0, 2_000_000);
    b.assert_zero_drift().await;
}

#[tokio::test]
async fn signed_underpayment_never_activates_or_queues_a_subscription() {
    let b = setup().await;
    let code = plan(&b).await;
    let valid = callback(&b, &code).await;
    let decoded: BTreeMap<String, String> = valid.query_pairs().into_owned().collect();
    let mut fields: BTreeMap<&str, String> = decoded
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "sign" | "sign_type"))
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    fields.insert("money", "0.01".to_owned());
    let mut invalid = valid.clone();
    invalid.set_query(None);
    invalid
        .query_pairs_mut()
        .extend_pairs(&fields)
        .append_pair("sign", &epay_sign(&fields))
        .append_pair("sign_type", "MD5");
    assert_eq!(
        reqwest::Client::new()
            .get(invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let grants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM subscription_grants WHERE user_id=$1")
            .bind(b.user_id)
            .fetch_one(&b.pg)
            .await
            .unwrap();
    assert_eq!(grants, 0);
    assert!(
        okapi_store::subscriptions::active_for_user(&b.pg, b.user_id)
            .await
            .unwrap()
            .is_none()
    );
    let state:(i16,i64) = sqlx::query_as("SELECT status,(SELECT count(*) FROM payment_receipts WHERE order_id=o.id) FROM recharge_orders o WHERE order_no=$1")
        .bind(&decoded["out_trade_no"]).fetch_one(&b.pg).await.unwrap();
    assert_eq!(state, (0, 0));
    assert_eq!(
        reqwest::Client::new()
            .get(valid)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(b.sub().await.0, 2_000_000);
    b.assert_zero_drift().await;
}
