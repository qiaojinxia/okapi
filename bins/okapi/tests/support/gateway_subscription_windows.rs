use super::*;
use std::time::Duration;

#[path = "gateway_subscription_images.rs"]
mod images;

// Each case isolates the worker sweep and financial history in its own database.
async fn database() -> TestResult<String> {
    let base = std::env::var("DATABASE_URL")?;
    let root = okapi_store::connect_pg(&base).await?;
    let suffix = Uuid::new_v4();
    let name = format!("okapi_window_{}", suffix.simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&root)
        .await?;
    root.close().await;
    let mut url = reqwest::Url::parse(&base)?;
    url.set_path(&name);
    let pg = okapi_store::connect_pg(url.as_str()).await?;
    okapi_store::run_migrations(&pg).await?;
    let random = u64::from_be_bytes(suffix.as_bytes()[..8].try_into()?);
    let first = i64::try_from(random & ((1_u64 << 47) - 1))? + 1_000_000_000_000;
    sqlx::query("SELECT setval('users_id_seq',$1,false)")
        .bind(first)
        .execute(&pg)
        .await?;
    pg.close().await;
    Ok(url.to_string())
}

async fn repair(b: &Bed) -> TestResult<okapi_store::history::Totals> {
    let mut guard = okapi_ledger::holds::UserGuard::acquire(&b.pg, b.uid).await?;
    let totals = okapi_store::history::totals(guard.connection()?, b.uid).await?;
    guard
        .repair(
            &b.ledger,
            Money::from_micros(totals.wallet),
            Money::from_micros(totals.subscription),
        )
        .await?;
    Ok(totals)
}

async fn near_window_end(b: &Bed, id: i64) -> TestResult<okapi_store::subscriptions::Subscription> {
    // Represent a full daily window whose expiry is shortly ahead of wall time.
    // A future-only roll would make repair treat the new window as unavailable.
    let end = chrono::Utc::now() + chrono::TimeDelta::seconds(5);
    let start = end - chrono::TimeDelta::days(1);
    sqlx::query(
        "UPDATE user_subscriptions SET starts_at=$2,window_start=$2,window_end=$3 WHERE id=$1",
    )
    .bind(id)
    .bind(start)
    .bind(end)
    .execute(&b.pg)
    .await?;
    repair(b).await?;
    Ok(okapi_store::subscriptions::by_id(&b.pg, id)
        .await?
        .ok_or("subscription")?)
}

async fn grant(
    b: &Bed,
    quota: i64,
    source: &str,
) -> TestResult<okapi_store::subscriptions::Subscription> {
    let code = format!("window-{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO plans(plan_code,display_name,grant_micro,kind,period,duration_days) VALUES($1,'Window contract', $2,1,1,30)")
        .bind(&code).bind(quota).execute(&b.pg).await?;
    let plan = okapi_store::subscriptions::find_sub_plan(&b.pg, &code)
        .await?
        .ok_or("plan")?;
    Ok(
        okapi_ledger::subscriptions::grant(&b.pg, &b.ledger, b.uid, &plan, source, "test:window")
            .await?
            .subscription()
            .clone(),
    )
}

#[derive(Clone, Copy)]
enum Change {
    Roll,
    Cancel,
    Replace,
}

async fn transition(
    bed: &Bed,
    sub: &okapi_store::subscriptions::Subscription,
    change: Change,
) -> TestResult<i64> {
    Ok(match change {
        Change::Roll => {
            let roll_at = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let now = chrono::Utc::now();
                    if now >= sub.window_end {
                        break now;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            let rolled =
                okapi_ledger::subscriptions::roll(&bed.pg, &bed.ledger, sub, roll_at, "test")
                    .await?;
            // The roll contract covers its supplied instant. Independent wall
            // clock reads can move during the awaited PG/Redis operations.
            assert!(
                rolled.window_start <= roll_at,
                "old={sub:?}, rolled={rolled:?}, at={roll_at}"
            );
            assert!(
                rolled.window_end > roll_at,
                "old={sub:?}, rolled={rolled:?}, at={roll_at}"
            );
            2000
        }
        Change::Cancel | Change::Replace => {
            okapi_ledger::subscriptions::end(&bed.pg, &bed.ledger, sub.id, 3, "test").await?;
            if matches!(change, Change::Replace) {
                grant(bed, 5000, "replacement").await?;
                5000
            } else {
                0
            }
        }
    })
}

async fn cross_window(failed: bool, change: Change, timeout: bool, lose_hot: bool) -> TestResult {
    let database = database().await?;
    let bed = Arc::new(Bed::new_at(false, &database).await?);
    okapi_ledger::pg::record_credit(
        &bed.pg,
        bed.uid,
        Money::from_micros(10_000_000),
        "adjust",
        "test",
        json!({}),
    )
    .await?;
    let sub = grant(&bed, 2000, "old").await?;
    let sub = if matches!(change, Change::Roll) {
        near_window_end(&bed, sub.id).await?
    } else {
        sub
    };
    bed.gate
        .mode
        .store(if failed { 2 } else { 1 }, Ordering::SeqCst);
    let call = {
        let bed = bed.clone();
        tokio::spawn(async move { bed.chat().await })
    };
    tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
    let reserved = bed.ledger.list_reservations(bed.uid).await?;
    assert_eq!(reserved.len(), 1);
    assert_eq!(reserved[0].pool, okapi_ledger::Pool::Subscription);
    assert_eq!(
        reserved[0].source_window.as_deref(),
        Some(format!("{}:{}", sub.id, sub.window_start.timestamp_micros()).as_str())
    );
    if matches!(change, Change::Roll) {
        assert!(
            chrono::Utc::now() < sub.window_end,
            "request must enter the old window"
        );
    }
    let expected = transition(&bed, &sub, change).await?;
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    if timeout {
        let deadline = chrono::DateTime::from_timestamp_millis(reserved[0].deadline_ms + 1)
            .ok_or("deadline")?;
        let swept =
            okapi::worker::sweep_expired_reservations(&bed.pg, &bed.ledger, deadline).await?;
        assert!(
            swept
                .iter()
                .any(|row| row.request_id == reserved[0].request_id)
        );
        assert_eq!(
            bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
            expected,
            "timeout returned an expired window's quota"
        );
    }
    if lose_hot {
        // Simulate cache loss after the PG period transition but before the
        // old provider result arrives. Recovery must use the captured period.
        bed.redis
            .del::<(), _>(format!("bal:{{{}}}", bed.uid))
            .await?;
    }
    bed.gate.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), call).await???;
    assert_eq!(status, if failed { 400 } else { 200 }, "{body}");
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(bed.pending.in_flight(), 0);
    let record: (i16, i16, i64) =
        sqlx::query_as("SELECT status,pool,amount_micro FROM billing_records WHERE request_id=$1")
            .bind(reserved[0].request_id)
            .fetch_one(&bed.pg)
            .await?;
    assert_eq!(record, if failed { (40, 1, 0) } else { (20, 1, 24) });
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected,
        "old request changed the new window's quota"
    );
    assert_eq!(bed.ledger.balance(bed.uid).await?.as_micros(), 10_000_000);
    let totals = {
        let mut connection = bed.pg.acquire().await?;
        okapi_store::history::totals(&mut connection, bed.uid).await?
    };
    assert_eq!(
        totals.subscription, expected,
        "financial history would recreate expired credit on repair"
    );
    repair(&bed).await?;
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    assert_new_window_usable(&bed, expected).await?;
    Ok(())
}

async fn assert_new_window_usable(bed: &Bed, expected: i64) -> TestResult {
    if expected > 0 {
        bed.gate.mode.store(0, Ordering::SeqCst);
        let (status, body) = bed.chat().await?;
        assert_eq!(status, 200, "{body}");
        bed.pending.wait_idle(Duration::from_secs(5)).await;
        assert_eq!(bed.pending.in_flight(), 0);
        assert_eq!(
            bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
            expected - 24,
            "new period remains usable and pays for its own call"
        );
        assert_eq!(repair(bed).await?.subscription, expected - 24);
    }
    Ok(())
}

#[tokio::test]
async fn old_success_does_not_change_new_window_quota() -> TestResult {
    cross_window(false, Change::Roll, false, false).await
}
#[tokio::test]
async fn old_failure_does_not_refill_new_window_quota() -> TestResult {
    cross_window(true, Change::Roll, false, false).await
}
#[tokio::test]
async fn cancellation_cannot_be_undone_by_a_late_refund() -> TestResult {
    cross_window(true, Change::Cancel, false, false).await
}
#[tokio::test]
async fn old_success_cannot_move_money_into_a_replacement_plan() -> TestResult {
    cross_window(false, Change::Replace, false, false).await
}
#[tokio::test]
async fn timeout_after_roll_does_not_refill_the_new_window() -> TestResult {
    cross_window(true, Change::Roll, true, false).await
}
#[tokio::test]
async fn success_after_expiry_sweep_keeps_original_window_ownership() -> TestResult {
    cross_window(false, Change::Roll, true, false).await
}

#[tokio::test]
async fn redis_loss_after_roll_keeps_late_success_in_its_original_window() -> TestResult {
    cross_window(false, Change::Roll, false, true).await
}

async fn same_window(failed: bool) -> TestResult {
    let database = database().await?;
    let bed = Bed::new_at(false, &database).await?;
    okapi_ledger::pg::record_credit(
        &bed.pg,
        bed.uid,
        Money::from_micros(10_000_000),
        "adjust",
        "test",
        json!({}),
    )
    .await?;
    grant(&bed, 2000, "current").await?;
    if failed {
        bed.gate.mode.store(2, Ordering::SeqCst);
        bed.gate.release.notify_one();
    }
    let (status, body) = bed.chat().await?;
    assert_eq!(status, if failed { 400 } else { 200 }, "{body}");
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(bed.pending.in_flight(), 0);
    let rows: Vec<(i16, i16, i64)> =
        sqlx::query_as("SELECT status,pool,amount_micro FROM billing_records WHERE user_id=$1")
            .bind(bed.uid)
            .fetch_all(&bed.pg)
            .await?;
    assert_eq!(rows, vec![if failed { (40, 1, 0) } else { (20, 1, 24) }]);
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    let expected = if failed { 2000 } else { 1976 };
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    assert_eq!(bed.ledger.balance(bed.uid).await?.as_micros(), 10_000_000);
    assert_eq!(repair(&bed).await?.subscription, expected);
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    Ok(())
}
#[tokio::test]
async fn current_window_success_uses_only_actual_charge() -> TestResult {
    same_window(false).await
}
#[tokio::test]
async fn current_window_failure_returns_its_own_quota() -> TestResult {
    same_window(true).await
}

async fn refund_request(
    bed: &Bed,
    database: &str,
    request_id: Uuid,
) -> TestResult<reqwest::RequestBuilder> {
    use sha2::{Digest, Sha256};
    let uid = okapi_store::provision::create_user(&bed.pg, "window-refund-admin").await?;
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(uid)
        .execute(&bed.pg)
        .await?;
    let token = format!("sk-window-admin-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(
        &bed.pg,
        uid,
        &hex::encode(Sha256::digest(token.as_bytes())),
        "sk-window",
    )
    .await?;
    let state = gateway::build_state(
        database,
        &std::env::var("OKAPI_REDIS_URL")?,
        "window-refund",
        None,
        None,
    )
    .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(
            listener,
            okapi::console::router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Ok(reqwest::Client::new()
        .post(format!("http://{address}/admin/billing/refund"))
        .bearer_auth(token)
        .json(&json!({"request_id":request_id,"reason":"period test"})))
}

async fn archive_bill(bed: &Bed, request_id: Uuid) -> TestResult {
    sqlx::query("CREATE TABLE billing_records_y2020m01 PARTITION OF billing_records FOR VALUES FROM ('2020-01-01T00:00:00Z') TO ('2020-02-01T00:00:00Z')").execute(&bed.pg).await?;
    sqlx::query("UPDATE billing_records SET created_at='2020-01-15T00:00:00Z' WHERE request_id=$1")
        .bind(request_id)
        .execute(&bed.pg)
        .await?;
    sqlx::query("INSERT INTO settings(key,value) VALUES('retention_months','12') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").execute(&bed.pg).await?;
    let dropped = okapi::worker::drop_expired_partitions(&bed.pg, chrono::Utc::now()).await?;
    assert!(dropped.iter().any(|v| v == "billing_records_y2020m01"));
    Ok(())
}

async fn refund_state(bed: &Bed) -> TestResult<Value> {
    Ok(sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'bills',(SELECT jsonb_agg(to_jsonb(r) ORDER BY request_id) FROM billing_financial_records r WHERE user_id=$1),
            'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY event_id) FROM billing_events e WHERE user_id=$1),
            'transfers',(SELECT jsonb_agg(to_jsonb(t) ORDER BY sequence) FROM fund_transfers t WHERE user_id=$1),
            'outbox',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM billing_outbox o WHERE payload->>'user_id'=$1::text),
            'used',(SELECT used_micro FROM api_keys WHERE id=$2))",
    ).bind(bed.uid).bind(bed.kid).fetch_one(&bed.pg).await?)
}

async fn failed_refund_is_atomic(bed: &Bed, request: &reqwest::RequestBuilder) -> TestResult {
    // Fail after status/refund event writes, before the expiry correction and
    // recovery intent can commit. The retried HTTP call below must work once.
    sqlx::query("ALTER TABLE billing_events ADD CONSTRAINT reject_window_refund CHECK (event_type <> 'sub_expire' OR payload->>'reason' IS DISTINCT FROM 'expired_window_admin_refund')")
        .execute(&bed.pg).await?;
    let before = refund_state(bed).await?;
    let key = format!("bal:{{{}}}", bed.uid);
    let hot: std::collections::BTreeMap<String, String> = bed.redis.hgetall(&key).await?;
    let response = request.try_clone().ok_or("request clone")?.send().await?;
    assert_eq!(response.status(), 500);
    assert_eq!(
        response.json::<Value>().await?["error"]["code"],
        "internal_error"
    );
    assert_eq!(refund_state(bed).await?, before, "partial PG refund");
    let after: std::collections::BTreeMap<String, String> = bed.redis.hgetall(&key).await?;
    assert_eq!(after, hot, "failed PG refund changed Redis");
    sqlx::query("ALTER TABLE billing_events DROP CONSTRAINT reject_window_refund")
        .execute(&bed.pg)
        .await?;
    Ok(())
}

async fn admin_refund_case(
    change: Option<Change>,
    archived: bool,
    elapsed: bool,
    inject_failure: bool,
) -> TestResult {
    let database = database().await?;
    let bed = Bed::new_at(false, &database).await?;
    okapi_ledger::pg::record_credit(
        &bed.pg,
        bed.uid,
        Money::from_micros(10_000_000),
        "adjust",
        "test",
        json!({}),
    )
    .await?;
    let sub = grant(&bed, 2000, "refundable").await?;
    let sub = if elapsed || matches!(change, Some(Change::Roll)) {
        near_window_end(&bed, sub.id).await?
    } else {
        sub
    };
    assert_eq!(bed.chat().await?.0, 200);
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(bed.pending.in_flight(), 0);
    let (rid, source): (Uuid, Option<String>) = sqlx::query_as(
        "SELECT request_id,source_window FROM billing_records WHERE user_id=$1 AND status=20",
    )
    .bind(bed.uid)
    .fetch_one(&bed.pg)
    .await?;
    assert_eq!(
        source.as_deref(),
        Some(format!("{}:{}", sub.id, sub.window_start.timestamp_micros()).as_str())
    );
    assert_eq!(bed.ledger.sub_balance(bed.uid).await?.0.as_micros(), 1976);
    if elapsed {
        tokio::time::timeout(Duration::from_secs(10), async {
            while chrono::Utc::now() < sub.window_end {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
    }
    let expected = if elapsed {
        1976
    } else if let Some(change) = change {
        transition(&bed, &sub, change).await?
    } else {
        2000
    };
    if archived {
        archive_bill(&bed, rid).await?;
    }
    let request = refund_request(&bed, &database, rid).await?;
    if inject_failure {
        failed_refund_is_atomic(&bed, &request).await?;
    }
    let response = request.try_clone().ok_or("request clone")?.send().await?;
    assert_eq!(response.status(), 200);
    let value: Value = response.json().await?;
    assert_eq!(value["outcome"], "refunded");
    assert_eq!(value["refunded_micro"], 24);
    assert_eq!(
        value["credited_micro"],
        if change.is_none() && !elapsed { 24 } else { 0 }
    );
    assert_eq!(value["pending"], false);
    let again = request.send().await?;
    assert_eq!(again.status(), 200);
    assert_eq!(again.json::<Value>().await?["outcome"], "already_refunded");
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    assert_eq!(bed.ledger.balance(bed.uid).await?.as_micros(), 10_000_000);
    let (status, stored): (i16, Option<String>) = sqlx::query_as(
        "SELECT status,source_window FROM billing_financial_records WHERE request_id=$1",
    )
    .bind(rid)
    .fetch_one(&bed.pg)
    .await?;
    assert_eq!((status, stored), (30, source));
    let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE id=$1")
        .bind(bed.kid)
        .fetch_one(&bed.pg)
        .await?;
    assert_eq!(used, 0);
    assert_refund_reversal(&bed, rid, archived).await?;
    assert_eq!(repair(&bed).await?.subscription, expected);
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    Ok(())
}
async fn assert_refund_reversal(bed: &Bed, rid: Uuid, archived: bool) -> TestResult {
    let reversals:Vec<Value>=sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.refunded' AND payload->>'request_id'=$1")
        .bind(rid.to_string()).fetch_all(&bed.pg).await?;
    assert_eq!(reversals.len(), 1);
    assert_eq!(reversals[0]["amount_micro"], -24);
    assert_eq!(reversals[0]["original_amount_micro"], -24);
    assert_eq!(reversals[0]["discount_micro"], 0);
    let upstream_cost: Option<i64> = if archived {
        sqlx::query_scalar(
            "SELECT upstream_cost_micro FROM billing_record_receipts WHERE request_id=$1",
        )
        .bind(rid)
        .fetch_one(&bed.pg)
        .await?
    } else {
        sqlx::query_scalar("SELECT upstream_cost_micro FROM billing_records WHERE request_id=$1")
            .bind(rid)
            .fetch_one(&bed.pg)
            .await?
    };
    assert_eq!(
        reversals[0]["upstream_cost_micro"],
        -upstream_cost.unwrap_or(0)
    );
    assert_eq!(reversals[0]["pool"], 1);
    Ok(())
}

#[tokio::test]
async fn admin_refund_of_current_window_restores_spendable_quota_once() -> TestResult {
    admin_refund_case(None, false, false, false).await
}
#[tokio::test]
async fn admin_refund_after_roll_does_not_increase_new_quota() -> TestResult {
    admin_refund_case(Some(Change::Roll), false, false, false).await
}
#[tokio::test]
async fn admin_refund_after_cancellation_keeps_subscription_closed() -> TestResult {
    admin_refund_case(Some(Change::Cancel), false, false, false).await
}
#[tokio::test]
async fn admin_refund_cannot_transfer_old_charge_into_a_replacement_plan() -> TestResult {
    admin_refund_case(Some(Change::Replace), false, false, false).await
}
#[tokio::test]
async fn archived_bill_keeps_window_identity_and_refunds_without_new_credit() -> TestResult {
    admin_refund_case(Some(Change::Replace), true, false, false).await
}

#[tokio::test]
async fn admin_refund_after_clock_expiry_does_not_restore_expired_quota() -> TestResult {
    admin_refund_case(None, false, true, false).await
}

#[tokio::test]
async fn expired_refund_failure_rolls_back_all_money_state_then_retries_once() -> TestResult {
    admin_refund_case(Some(Change::Replace), false, false, true).await
}

#[tokio::test]
async fn archived_expired_refund_failure_keeps_receipt_refundable() -> TestResult {
    admin_refund_case(Some(Change::Replace), true, false, true).await
}

async fn legacy_receipt_blocks_transition_until_closed(failed: bool) -> TestResult {
    let database = database().await?;
    let bed = Arc::new(Bed::new_at(false, &database).await?);
    okapi_ledger::pg::record_credit(
        &bed.pg,
        bed.uid,
        Money::from_micros(10_000_000),
        "adjust",
        "test",
        json!({}),
    )
    .await?;
    let sub = grant(&bed, 2000, "legacy").await?;
    bed.gate
        .mode
        .store(if failed { 2 } else { 1 }, Ordering::SeqCst);
    let call = {
        let bed = bed.clone();
        tokio::spawn(async move { bed.chat().await })
    };
    tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
    let reserved = bed.ledger.list_reservations(bed.uid).await?;
    assert_eq!(reserved.len(), 1);
    assert!(reserved[0].source_window.is_some());
    let key = format!("bal:{{{}}}", bed.uid);
    let field = format!("r:{}", reserved[0].request_id);
    let raw: String = bed.redis.hget(&key, &field).await?;
    bed.redis
        .hset::<(), _, _>(
            &key,
            (&field, raw.split('|').take(4).collect::<Vec<_>>().join("|")),
        )
        .await?;
    // Repair must preserve the unknown legacy identity, not silently bless it.
    repair(&bed).await?;
    assert!(
        bed.ledger.list_reservations(bed.uid).await?[0]
            .source_window
            .is_none()
    );
    let ended = okapi_ledger::subscriptions::end(&bed.pg, &bed.ledger, sub.id, 3, "test").await;
    assert!(matches!(
        ended,
        Err(okapi_ledger::LedgerError::HoldRecoveryRequired)
    ));
    let status: i16 = sqlx::query_scalar("SELECT status FROM user_subscriptions WHERE id=$1")
        .bind(sub.id)
        .fetch_one(&bed.pg)
        .await?;
    assert_eq!(status, 1);
    assert_eq!(bed.ledger.sub_balance(bed.uid).await?.0.as_micros(), 1952);
    bed.gate.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), call).await???;
    assert_eq!(status, if failed { 400 } else { 200 }, "{body}");
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(bed.pending.in_flight(), 0);
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    assert_eq!(
        repair(&bed).await?.subscription,
        if failed { 2000 } else { 1976 }
    );
    okapi_ledger::subscriptions::end(&bed.pg, &bed.ledger, sub.id, 3, "test").await?;
    assert_eq!(bed.ledger.sub_balance(bed.uid).await?.0.as_micros(), 0);
    assert_eq!(repair(&bed).await?.subscription, 0);
    assert_eq!(bed.ledger.balance(bed.uid).await?.as_micros(), 10_000_000);
    Ok(())
}

#[tokio::test]
async fn legacy_success_closes_before_subscription_can_end() -> TestResult {
    legacy_receipt_blocks_transition_until_closed(false).await
}

#[tokio::test]
async fn legacy_failure_closes_before_subscription_can_end() -> TestResult {
    legacy_receipt_blocks_transition_until_closed(true).await
}

async fn request_finishes_after_clock_expiry(failed: bool) -> TestResult {
    let database = database().await?;
    let bed = Arc::new(Bed::new_at(false, &database).await?);
    okapi_ledger::pg::record_credit(
        &bed.pg,
        bed.uid,
        Money::from_micros(10_000_000),
        "adjust",
        "test",
        json!({}),
    )
    .await?;
    let sub = grant(&bed, 2000, "elapsed").await?;
    let sub = near_window_end(&bed, sub.id).await?;
    bed.gate
        .mode
        .store(if failed { 2 } else { 1 }, Ordering::SeqCst);
    let call = {
        let bed = bed.clone();
        tokio::spawn(async move { bed.chat().await })
    };
    tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
    assert!(chrono::Utc::now() < sub.window_end);
    let before = bed.ledger.list_reservations(bed.uid).await?;
    assert_eq!(before.len(), 1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while chrono::Utc::now() < sub.window_end {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    // The clock expired, but no worker has changed the recorded period.
    // Repair must neither forget the receipt nor turn it into a new period.
    repair(&bed).await?;
    assert_eq!(
        bed.ledger.subscription_window(bed.uid).await?,
        before[0].source_window
    );
    assert_eq!(bed.ledger.sub_balance(bed.uid).await?.0.as_micros(), 1952);
    bed.gate.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), call).await???;
    assert_eq!(status, if failed { 400 } else { 200 }, "{body}");
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(bed.pending.in_flight(), 0);
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    let expected = if failed { 2000 } else { 1976 };
    assert_eq!(repair(&bed).await?.subscription, expected);
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    bed.gate.mode.store(0, Ordering::SeqCst);
    assert_eq!(bed.chat().await?.0, 200);
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(bed.pending.in_flight(), 0);
    let totals = repair(&bed).await?;
    assert_eq!(
        totals.wallet, 9_999_976,
        "new request must use the wallet after expiry"
    );
    assert_eq!(totals.subscription, expected);
    assert_eq!(bed.ledger.balance(bed.uid).await?.as_micros(), 9_999_976);
    Ok(())
}

#[tokio::test]
async fn elapsed_window_success_survives_repair_without_charging_a_different_pool() -> TestResult {
    request_finishes_after_clock_expiry(false).await
}

#[tokio::test]
async fn elapsed_window_failure_cannot_make_expired_credit_spendable() -> TestResult {
    request_finishes_after_clock_expiry(true).await
}
