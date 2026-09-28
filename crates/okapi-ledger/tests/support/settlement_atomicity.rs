//! Closing a reservation must either fully succeed or preserve the recovery record.
use super::*;
use fred::{interfaces::LuaInterface, types::Value};
use okapi_ledger::CommitOutcome;

const MAXIMUM: i64 = 9_007_199_254_740_991;
#[path = "settlement_edges.rs"]
mod edges;

fn conc(bed: &Bed, key: i64) -> String {
    format!("conc:{{{}}}:k:{key}", bed.request.user_id)
}

async fn snapshot(bed: &Bed) -> TestResult<Vec<Value>> {
    Ok(bed.redis.eval(
        "return {redis.call('DUMP',KEYS[1]),redis.call('DUMP',KEYS[2]),redis.call('DUMP',KEYS[3])}",
        vec![bed.balance_key(), conc(bed, 7), conc(bed, 8)], Vec::<String>::new(),
    ).await?)
}

async fn held(subscription: bool, amount: i64) -> TestResult<Bed> {
    let mut bed = Bed::new().await?;
    bed.request.est = Money::from_micros(amount);
    bed.redis
        .hset::<(), _, _>(bed.balance_key(), ("avail", MAXIMUM))
        .await?;
    if subscription {
        bed.ledger
            .sub_set(
                bed.request.user_id,
                Money::from_micros(amount.max(10_000)),
                bed.now.timestamp() + 3600,
            )
            .await?;
    }
    assert!(matches!(
        bed.ledger.reserve(bed.request, bed.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    Ok(bed)
}

async fn close(bed: &Bed, key: i64, actual: Option<i64>) -> Result<i64, LedgerError> {
    if let Some(amount) = actual {
        let result = bed
            .ledger
            .commit(
                bed.request.user_id,
                key,
                bed.request.request_id,
                Money::from_micros(amount),
            )
            .await?;
        Ok(match result {
            CommitOutcome::Committed { refund_delta, .. } => refund_delta.as_micros(),
            CommitOutcome::NoReservation => 0,
        })
    } else {
        let result = bed
            .ledger
            .refund(bed.request.user_id, key, bed.request.request_id)
            .await?;
        Ok(result.released.as_micros())
    }
}

async fn reject_unchanged(bed: &Bed, key: i64, actual: Option<i64>) -> TestResult {
    let before = snapshot(bed).await?;
    assert!(
        close(bed, key, actual).await.is_err(),
        "invalid closure accepted: {actual:?}"
    );
    assert_eq!(
        snapshot(bed).await?,
        before,
        "failed closure changed money, receipt or concurrency"
    );
    Ok(())
}

#[tokio::test]
async fn corrupted_concurrency_cannot_partially_commit_or_refund() -> TestResult {
    for subscription in [false, true] {
        for actual in [Some(700), None] {
            let bed = held(subscription, 1_000).await?;
            bed.redis.del::<(), _>(conc(&bed, 7)).await?;
            bed.redis
                .hset::<(), _, _>(conc(&bed, 7), ("bad", "type"))
                .await?;
            reject_unchanged(&bed, 7, actual).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn safe_large_amounts_close_with_exact_receipts() -> TestResult {
    for subscription in [false, true] {
        for actual in [Some(1), None] {
            let bed = held(subscription, MAXIMUM).await?;
            assert_eq!(close(&bed, 7, actual).await?, MAXIMUM - actual.unwrap_or(0));
            let field = if subscription { "sub" } else { "avail" };
            let balance: i64 = bed.redis.hget(bed.balance_key(), field).await?;
            assert_eq!(balance, MAXIMUM - actual.unwrap_or(0));
            assert!(
                bed.ledger
                    .list_reservations(bed.request.user_id)
                    .await?
                    .is_empty()
            );
            let count: i64 = bed.redis.get(conc(&bed, 7)).await?;
            assert_eq!(count, 0);
        }
    }
    Ok(())
}

#[tokio::test]
async fn negative_or_unsafe_actual_amount_cannot_credit_or_close() -> TestResult {
    for actual in [-1, MAXIMUM + 1, i64::MAX] {
        let bed = held(false, 1_000).await?;
        reject_unchanged(&bed, 7, Some(actual)).await?;
    }
    Ok(())
}

#[tokio::test]
async fn mismatched_key_cannot_release_another_keys_slot() -> TestResult {
    for actual in [Some(700), None] {
        let bed = held(false, 1_000).await?;
        bed.redis
            .set::<(), _, _>(conc(&bed, 8), "3", None, None, false)
            .await?;
        reject_unchanged(&bed, 8, actual).await?;
    }
    Ok(())
}

#[tokio::test]
async fn malformed_reservation_pool_or_fields_are_not_guessed() -> TestResult {
    for raw in [
        "1000|123|7|2",
        "1000|123|7|",
        "1000|123||0",
        "1000|123|7|0|extra",
        "1000|123|7|0|w:42:100",
        "1000|123|7|1|w:",
        "1000|123|7|1|w:42 100",
        "1000|123|7|1|w:雪",
        "1000|123|7|1|w:42:100|extra",
        "-1000|123|7|0",
        "1000|bad|7|0",
        "01|123|7|0",
    ] {
        for actual in [Some(700), None] {
            let bed = held(false, 1_000).await?;
            bed.redis
                .hset::<(), _, _>(
                    bed.balance_key(),
                    (format!("r:{}", bed.request.request_id), raw),
                )
                .await?;
            reject_unchanged(&bed, 7, actual).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn period_identity_conflicts_preserve_receipts_and_old_closures_release_slots_once()
-> TestResult {
    for same_window in [false, true] {
        for actual in [Some(700), Some(2000), None] {
            let bed = held(true, 1000).await?;
            let field = format!("r:{}", bed.request.request_id);
            let raw: String = bed.redis.hget(bed.balance_key(), &field).await?;
            let epoch = if same_window { "42:100" } else { "43:200" };
            let balance = if same_window { 9000 } else { 5000 };
            bed.redis
                .hset::<(), _, _>(
                    bed.balance_key(),
                    vec![
                        (field, format!("{raw}|w:42:100")),
                        ("sub_epoch".into(), epoch.into()),
                        ("sub".into(), balance.to_string()),
                    ],
                )
                .await?;
            let before = snapshot(&bed).await?;
            let wrong = bed
                .ledger
                .commit_in_source(
                    bed.request.user_id,
                    7,
                    bed.request.request_id,
                    Money::from_micros(actual.unwrap_or(0)),
                    Pool::Subscription,
                    Some("44:300"),
                )
                .await;
            assert!(matches!(wrong, Err(LedgerError::ReservationConflict)));
            assert_eq!(snapshot(&bed).await?, before);
            let expected = if same_window {
                1000 - actual.unwrap_or(0)
            } else {
                0
            };
            if let Some(actual) = actual {
                let outcome = bed
                    .ledger
                    .commit_in_source(
                        bed.request.user_id,
                        7,
                        bed.request.request_id,
                        Money::from_micros(actual),
                        Pool::Subscription,
                        Some("42:100"),
                    )
                    .await?;
                assert!(
                    matches!(outcome, CommitOutcome::Committed { refund_delta, pool: Pool::Subscription, .. } if refund_delta.as_micros()==expected)
                );
            } else {
                let outcome = bed
                    .ledger
                    .refund(bed.request.user_id, 7, bed.request.request_id)
                    .await?;
                assert!(outcome.closed);
                assert_eq!(outcome.released.as_micros(), expected);
                assert_eq!(outcome.pool, Pool::Subscription);
            }
            assert_eq!(
                bed.ledger
                    .sub_balance(bed.request.user_id)
                    .await?
                    .0
                    .as_micros(),
                balance + expected
            );
            assert_eq!(
                bed.ledger.balance(bed.request.user_id).await?.as_micros(),
                MAXIMUM
            );
            let count: i64 = bed.redis.get(conc(&bed, 7)).await?;
            assert_eq!(count, 0);
            let closed = snapshot(&bed).await?;
            let again = bed
                .ledger
                .refund(bed.request.user_id, 7, bed.request.request_id)
                .await?;
            assert!(!again.closed);
            assert_eq!(again.released, Money::ZERO);
            assert_eq!(snapshot(&bed).await?, closed);
        }
    }
    Ok(())
}

#[tokio::test]
async fn restored_counter_can_retry_same_receipt_once() -> TestResult {
    for actual in [Some(700), None] {
        let bed = held(true, 1_000).await?;
        bed.redis
            .set::<(), _, _>(conc(&bed, 7), "bad", None, None, false)
            .await?;
        reject_unchanged(&bed, 7, actual).await?;
        bed.redis
            .set::<(), _, _>(conc(&bed, 7), "1", None, None, false)
            .await?;
        close(&bed, 7, actual).await?;
        let before = snapshot(&bed).await?;
        close(&bed, 7, actual).await?;
        assert_eq!(snapshot(&bed).await?, before);
        assert!(matches!(
            bed.ledger
                .commit(bed.request.user_id, 7, bed.request.request_id, Money::ZERO)
                .await?,
            CommitOutcome::NoReservation
        ));
    }
    Ok(())
}

#[tokio::test]
async fn large_subscription_quota_returns_and_stores_exact_integers() -> TestResult {
    let bed = Bed::new().await?;
    let result = bed
        .ledger
        .sub_set(
            bed.request.user_id,
            Money::from_micros(MAXIMUM),
            bed.now.timestamp() + 3600,
        )
        .await?;
    assert_eq!(result.before, Money::ZERO);
    assert_eq!(result.after.as_micros(), MAXIMUM);
    let raw: String = bed.redis.hget(bed.balance_key(), "sub").await?;
    assert_eq!(raw, MAXIMUM.to_string());
    Ok(())
}
