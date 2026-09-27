//! Real PG/Redis contracts: durable freezes, accounting evidence, replay and window ownership.
#[path = "support/native_batch_jobs.rs"]
mod batches;
#[path = "support/holds_concurrency.rs"]
mod concurrency;
#[path = "support/holds_recovery.rs"]
mod recovery;
#[path = "support/holds.rs"]
mod support;
use chrono::TimeDelta;
use okapi_domain::{BillingState, Money};
use okapi_ledger::{
    LedgerError, ReserveOutcome,
    holds::{self, Admission, Status, UserGuard},
};
use support::{Bed, TestResult};
use uuid::Uuid;

#[tokio::test]
async fn legacy_receipts_without_cache_reporting_flags_replay_without_rebilling() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    // Simulate the receipt written before additive collection-status fields existed.
    sqlx::query("UPDATE balance_holds SET settlement=settlement #- '{usage,cache_read_reported}' #- '{usage,cache_write_reported}' WHERE id=$1")
        .bind(id).execute(&bed.pg).await?;
    let legacy = bed.row(id).await?.settlement;
    let balance = bed.snapshot().await?;
    for _ in 0..2 {
        let replay = holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
        assert_eq!(replay.status, Status::Closed);
        assert_eq!(
            replay.settlement, legacy,
            "replay must not rewrite the original receipt"
        );
    }
    for variant in 0..3 {
        let mut changed = bed.bill(id, 1_000)?;
        match variant {
            0 => changed.usage.prompt_tokens = 1,
            1 => changed.usage.cache_read_reported = true,
            _ => changed.usage.cache_write_reported = true,
        }
        assert!(matches!(
            holds::settle(&bed.pg, &bed.ledger, changed, bed.now).await,
            Err(LedgerError::HoldConflict)
        ));
    }
    assert_eq!(bed.snapshot().await?, balance);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn wallet_freeze_coexists_with_regular_requests_and_charges_once() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    assert_eq!(bed.wallet().await?, 8_500);
    assert_eq!(bed.event_total(0).await?, 10_000, "freeze is not expense");
    let regular = bed.regular(Uuid::new_v4());
    assert!(matches!(
        bed.ledger.reserve(regular, bed.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    assert_eq!(bed.wallet().await?, 7_500);
    let closed = holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(closed.status, Status::Closed);
    assert_eq!(closed.credit_micro, Some(500));
    assert_eq!(bed.wallet().await?, 8_000);
    bed.ledger
        .refund(bed.uid, bed.kid, regular.request_id)
        .await?;
    assert_eq!(bed.wallet().await?, 9_000);
    assert_eq!(bed.event_total(0).await?, 9_000);
    bed.evidence(id, 1_000).await?;
    assert_eq!(
        holds::inflight(&bed.ledger, bed.uid).await?,
        (Money::ZERO, Money::ZERO)
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_same_identity_freezes_once_and_concurrent_settlement_refunds_once() -> TestResult
{
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let b = bed.clone();
        tasks.spawn(
            async move { holds::reserve(&b.pg, &b.ledger, b.request(id, 1_500), b.now).await },
        );
    }
    let mut fresh = 0;
    while let Some(result) = tasks.join_next().await {
        match result?? {
            Admission::Held { replayed, .. } => fresh += usize::from(!replayed),
            other => return Err(format!("unexpected admission {other:?}").into()),
        }
    }
    assert_eq!(fresh, 1);
    assert_eq!(bed.wallet().await?, 8_500);
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM billing_events WHERE request_id=$1 AND event_type='reserve'",
    )
    .bind(id)
    .fetch_one(&bed.pg)
    .await?;
    assert_eq!(count, 1);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let b = bed.clone();
        let bill = b.bill(id, 1_000)?;
        tasks.spawn(async move { holds::settle(&b.pg, &b.ledger, bill, b.now).await });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result??.status, Status::Closed);
    }
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn distinct_admissions_cannot_spend_already_frozen_wallet() -> TestResult {
    let bed = Bed::new().await?;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let b = bed.clone();
        tasks.spawn(async move {
            holds::reserve(&b.pg, &b.ledger, b.request(Uuid::new_v4(), 1_500), b.now).await
        });
    }
    let (mut held, mut denied) = (0, 0);
    while let Some(result) = tasks.join_next().await {
        match result?? {
            Admission::Held { .. } => held += 1,
            Admission::Insufficient { balance } => {
                denied += 1;
                assert_eq!(balance.as_micros(), 1_000);
            }
            other @ (Admission::Closed(_) | Admission::ConcurrencyLimited) => {
                return Err(format!("unexpected {other:?}").into());
            }
        }
    }
    assert_eq!((held, denied), (6, 4));
    assert_eq!(bed.wallet().await?, 1_000);
    assert_eq!(
        holds::inflight(&bed.ledger, bed.uid).await?.0.as_micros(),
        9_000
    );
    assert_eq!(bed.event_total(0).await?, 10_000);
    Ok(())
}

#[tokio::test]
async fn changed_identity_price_owner_or_settlement_cannot_move_money() -> TestResult {
    let bed = Bed::new().await?;
    let other = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let before = bed.snapshot().await?;
    let mut price = bed.pricing.clone();
    price.epoch += 1;
    for variant in 0..5 {
        let mut req = bed.request(id, 1_500);
        match variant {
            0 => req.maximum = Money::from_micros(1_000),
            1 => req.api_key_id = other.kid,
            2 => {
                req.request_hash =
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
            }
            3 => req.pricing = &price,
            _ => req = other.request(id, 1_500),
        }
        assert!(matches!(
            holds::reserve(&bed.pg, &bed.ledger, req, bed.now).await,
            Err(LedgerError::HoldConflict)
        ));
    }
    assert_eq!(bed.snapshot().await?, before);
    assert_eq!(other.wallet().await?, 10_000);
    for variant in 0..4 {
        let mut bill = bed.bill(id, 1_000)?;
        match variant {
            0 => bill.amount = Money::from_micros(1_501),
            1 => bill.api_key_id = other.kid,
            2 => bill.pricing_snapshot.as_mut().ok_or("pricing")?["media_units"] = 4.into(),
            _ => bill.pricing_snapshot.as_mut().ok_or("pricing")?["group_ratio"] = 2.into(),
        }
        assert!(
            holds::settle(&bed.pg, &bed.ledger, bill, bed.now)
                .await
                .is_err()
        );
        assert_eq!(bed.snapshot().await?, before);
    }
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert!(matches!(
        holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 500)?, bed.now).await,
        Err(LedgerError::HoldConflict)
    ));
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_500), bed.now).await?,
        Admission::Closed(_)
    ));
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn failed_and_cancelled_jobs_refund_once() -> TestResult {
    for state in [BillingState::Failed, BillingState::Refunded] {
        let bed = Bed::new().await?;
        let id = Uuid::new_v4();
        bed.reserve(id).await?;
        let mut bill = bed.bill(id, 0)?;
        bill.state = state;
        for _ in 0..3 {
            holds::settle(&bed.pg, &bed.ledger, bill.clone(), bed.now).await?;
        }
        assert_eq!(bed.wallet().await?, 10_000);
        assert_eq!(bed.event_total(0).await?, 10_000);
        bed.evidence(id, 0).await?;
    }
    Ok(())
}

#[tokio::test]
async fn zero_cost_jobs_remain_idempotent() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    for _ in 0..2 {
        assert!(matches!(
            holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 0), bed.now).await?,
            Admission::Held { .. }
        ));
    }
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 0)?, bed.now).await?;
    assert_eq!(bed.wallet().await?, 10_000);
    bed.evidence(id, 0).await
}

#[tokio::test]
async fn current_subscription_window_refunds_and_renewal_keeps_ownership() -> TestResult {
    let bed = Bed::new().await?;
    let (plan, _) = bed.subscription().await?;
    let id = Uuid::new_v4();
    let hold = bed.reserve(id).await?;
    assert_eq!(hold.pool, Some(1));
    assert_eq!(bed.sub().await?, 500);
    okapi_ledger::subscriptions::grant(&bed.pg, &bed.ledger, bed.uid, &plan, "test:renew", "test")
        .await?;
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(bed.sub().await?, 1_000);
    assert_eq!(bed.event_total(1).await?, 1_000);
    assert_eq!(bed.wallet().await?, 10_000);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn expired_window_refund_does_not_top_up_a_new_window() -> TestResult {
    let bed = Bed::new().await?;
    let (_, sub) = bed.subscription().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let later = sub
        .window_end
        .checked_add_signed(TimeDelta::seconds(1))
        .ok_or("time")?;
    okapi_ledger::subscriptions::roll(&bed.pg, &bed.ledger, &sub, later, "test").await?;
    assert_eq!(bed.sub().await?, 2_000);
    assert_eq!(bed.event_total(1).await?, 3_500);
    let closed = holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, later).await?;
    assert_eq!(closed.credit_micro, Some(0));
    assert_eq!(bed.sub().await?, 2_000);
    assert_eq!(bed.event_total(1).await?, 2_000);
    assert_eq!(bed.wallet().await?, 10_000);
    let expired: i64 = sqlx::query_scalar(
        "SELECT delta_micro FROM billing_events WHERE request_id=$1 AND event_type='sub_expire'",
    )
    .bind(id)
    .fetch_one(&bed.pg)
    .await?;
    assert_eq!(expired, -500);
    Ok(())
}

#[tokio::test]
async fn one_connection_pool_supports_freeze_settle_subscription_and_repair() -> TestResult {
    let bed = Bed::with_pool(1).await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    bed.subscription().await?;
    bed.repair().await?;
    assert_eq!(bed.wallet().await?, 9_000);
    assert_eq!(bed.sub().await?, 2_000);
    let guard = UserGuard::acquire(&bed.pg, bed.uid).await?;
    drop(guard);
    let _again = UserGuard::acquire(&bed.pg, bed.uid).await?;
    Ok(())
}

#[tokio::test]
async fn pending_capacity_is_bounded_and_cancellation_releases_a_slot() -> TestResult {
    let bed = Bed::new().await?;
    let first = Uuid::new_v4();
    for i in 0..128 {
        let id = if i == 0 { first } else { Uuid::new_v4() };
        assert!(matches!(
            holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 20_000), bed.now).await?,
            Admission::Insufficient { .. }
        ));
    }
    let another = Uuid::new_v4();
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(another, 20_000), bed.now).await,
        Err(LedgerError::HoldCapacity)
    ));
    let mut bill = bed.bill(first, 0)?;
    bill.state = BillingState::Refunded;
    holds::settle(&bed.pg, &bed.ledger, bill, bed.now).await?;
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(another, 20_000), bed.now).await?,
        Admission::Insufficient { .. }
    ));
    assert_eq!(bed.wallet().await?, 10_000);
    Ok(())
}

#[tokio::test]
async fn a_frozen_surcharge_uses_negative_discount_without_changing_the_pricing_contract()
-> TestResult {
    let mut bed = Bed::new().await?;
    bed.pricing.user_multiplier =
        okapi_pricing::RatioFp::from_scaled(1_200_000).ok_or("test ratio")?;
    let id = Uuid::new_v4();
    // Three images at 500 micro with a 1.2 multiplier; two images eventually succeed.
    holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_800), bed.now).await?;
    let mut bill = bed.bill(id, 1_200)?;
    bill.original = Money::from_micros(1_000);
    bill.list_price = Money::from_micros(1_000);
    bill.discount = Money::from_micros(-200);
    for _ in 0..2 {
        holds::settle(&bed.pg, &bed.ledger, bill.clone(), bed.now).await?;
    }
    assert_eq!(bed.wallet().await?, 8_800);
    assert_eq!(bed.event_total(0).await?, 8_800);
    let record: (i64,i64,i64,Option<i64>) = sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro FROM billing_records WHERE request_id=$1")
        .bind(id).fetch_one(&bed.pg).await?;
    assert_eq!(record, (1_200, 1_000, -200, Some(600)));
    let outbox: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1")
            .bind(id.to_string())
            .fetch_all(&bed.pg)
            .await?;
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0]["discount_micro"], -200);
    Ok(())
}

#[tokio::test]
async fn cold_subscription_window_reset_preserves_durable_frozen_money() -> TestResult {
    use fred::interfaces::KeysInterface;
    let bed = Bed::new().await?;
    let (_, sub) = bed.subscription().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    bed.redis
        .del::<(), _>(format!("bal:{{{}}}", bed.uid))
        .await?;
    let later = sub
        .window_end
        .checked_add_signed(TimeDelta::seconds(1))
        .ok_or("time")?;
    okapi_ledger::subscriptions::roll(&bed.pg, &bed.ledger, &sub, later, "test").await?;
    assert_eq!(bed.sub().await?, 2000);
    assert_eq!(bed.event_total(1).await?, 3500);
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1000)?, later).await?;
    assert_eq!(bed.sub().await?, 2000);
    assert_eq!(bed.event_total(1).await?, 2000);
    Ok(())
}
