use super::*;
use std::sync::Arc;

#[tokio::test]
async fn invalid_counter_numbers_and_result_overflow_leave_receipts_intact() -> TestResult {
    for actual in [Some(700), None] {
        for value in ["bad", "01", "1.0", "-1", "9007199254740992"] {
            let bed = held(false, 1_000).await?;
            bed.redis
                .set::<(), _, _>(conc(&bed, 7), value, None, None, false)
                .await?;
            reject_unchanged(&bed, 7, actual).await?;
        }
        for balance in [MAXIMUM.to_string(), "01".into(), "9007199254740992".into()] {
            let bed = held(false, 1_000).await?;
            bed.redis
                .hset::<(), _, _>(bed.balance_key(), ("avail", balance))
                .await?;
            reject_unchanged(&bed, 7, actual).await?;
        }
    }
    let bed = held(false, 1_000).await?;
    bed.redis
        .hset::<(), _, _>(bed.balance_key(), ("avail", -MAXIMUM))
        .await?;
    reject_unchanged(&bed, 7, Some(2_000)).await
}

#[tokio::test]
async fn extra_charges_can_reach_the_negative_safe_boundary_exactly() -> TestResult {
    for subscription in [false, true] {
        let bed = held(subscription, 0).await?;
        let field = if subscription { "sub" } else { "avail" };
        bed.redis
            .hset::<(), _, _>(bed.balance_key(), (field, "0"))
            .await?;
        assert_eq!(close(&bed, 7, Some(MAXIMUM)).await?, -MAXIMUM);
        let balance: String = bed.redis.hget(bed.balance_key(), field).await?;
        assert_eq!(balance, (-MAXIMUM).to_string());
        assert!(
            bed.ledger
                .list_reservations(bed.request.user_id)
                .await?
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
async fn legacy_receipts_and_full_bigint_key_identity_remain_supported() -> TestResult {
    for key in [0, 7, i64::MAX] {
        for actual in [Some(700), None] {
            let mut bed = Bed::new().await?;
            bed.request.api_key_id = key;
            bed.ledger.reserve(bed.request, bed.now).await?;
            let deadline = bed.now.timestamp_millis() + 600_000;
            let legacy = if key == 0 {
                format!("1000|{deadline}")
            } else {
                format!("1000|{deadline}|{key}")
            };
            bed.redis
                .hset::<(), _, _>(
                    bed.balance_key(),
                    (format!("r:{}", bed.request.request_id), legacy),
                )
                .await?;
            let released = close(&bed, key, actual).await?;
            assert_eq!(released, 1_000 - actual.unwrap_or(0));
            assert_eq!(
                bed.ledger.balance(bed.request.user_id).await?.as_micros(),
                10_000 - actual.unwrap_or(0)
            );
            let count: i64 = bed.redis.get(conc(&bed, key)).await?;
            assert_eq!(count, 0);
        }
    }
    Ok(())
}

#[tokio::test]
async fn subscription_reset_rejects_bad_or_overflowing_state_before_changing_window() -> TestResult
{
    let bed = held(true, 1_000).await?;
    for (quota, until) in [(-1, 123), (MAXIMUM, 123), (100, -1), (MAXIMUM + 1, 123)] {
        let before = snapshot(&bed).await?;
        assert!(
            bed.ledger
                .sub_set_window(
                    bed.request.user_id,
                    Money::from_micros(quota),
                    until,
                    "new-window"
                )
                .await
                .is_err()
        );
        assert_eq!(snapshot(&bed).await?, before);
    }
    let result = bed
        .ledger
        .sub_set_window(
            bed.request.user_id,
            Money::from_micros(MAXIMUM - 1_000),
            bed.now.timestamp() + 3600,
            "new-window",
        )
        .await?;
    assert_eq!(result.before.as_micros(), 9_000);
    assert_eq!(result.after.as_micros(), MAXIMUM);
    let epoch: String = bed.redis.hget(bed.balance_key(), "sub_epoch").await?;
    assert_eq!(epoch, "new-window");
    bed.redis
        .hset::<(), _, _>(
            bed.balance_key(),
            (format!("r:{}", bed.request.request_id), "1000|123|7|9"),
        )
        .await?;
    let before = snapshot(&bed).await?;
    assert!(
        bed.ledger
            .sub_set(bed.request.user_id, Money::ZERO, 0)
            .await
            .is_err()
    );
    assert_eq!(snapshot(&bed).await?, before);
    Ok(())
}

#[tokio::test]
async fn concurrent_commit_and_refund_close_once_without_negative_slots() -> TestResult {
    for subscription in [false, true] {
        let bed = Arc::new(held(subscription, 1_000).await?);
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..32 {
            let bed = bed.clone();
            tasks.spawn(async move { close(&bed, 7, (index % 2 == 0).then_some(700)).await });
        }
        let mut nonzero = Vec::new();
        while let Some(result) = tasks.join_next().await {
            let released = result??;
            if released != 0 {
                nonzero.push(released);
            }
        }
        assert_eq!(nonzero.len(), 1);
        assert!(matches!(nonzero[0], 300 | 1000));
        let field = if subscription { "sub" } else { "avail" };
        let balance: i64 = bed.redis.hget(bed.balance_key(), field).await?;
        assert_eq!(
            balance,
            if subscription { 9_000 } else { MAXIMUM - 1_000 } + nonzero[0]
        );
        let count: i64 = bed.redis.get(conc(&bed, 7)).await?;
        assert_eq!(count, 0);
        assert!(
            bed.ledger
                .list_reservations(bed.request.user_id)
                .await?
                .is_empty()
        );
    }
    Ok(())
}
