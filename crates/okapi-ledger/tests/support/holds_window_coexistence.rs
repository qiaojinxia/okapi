use super::*;
use fred::interfaces::KeysInterface;
use okapi_ledger::{Pool, SettlementInput};
use std::time::Duration;

#[derive(Clone, Copy)]
enum Change {
    Current,
    Roll,
    Cancel,
    Replace,
}

async fn window(bed: &Bed, change: Change) -> TestResult<okapi_store::subscriptions::Subscription> {
    let (_, sub) = bed.subscription().await?;
    if !matches!(change, Change::Roll) {
        return Ok(sub);
    }
    let end = chrono::Utc::now() + TimeDelta::seconds(5);
    let start = end - TimeDelta::days(1);
    sqlx::query(
        "UPDATE user_subscriptions SET starts_at=$2,window_start=$2,window_end=$3 WHERE id=$1",
    )
    .bind(sub.id)
    .bind(start)
    .bind(end)
    .execute(&bed.pg)
    .await?;
    bed.repair().await?;
    Ok(okapi_store::subscriptions::by_id(&bed.pg, sub.id)
        .await?
        .ok_or("window")?)
}

async fn change_window(
    bed: &Bed,
    sub: &okapi_store::subscriptions::Subscription,
    change: Change,
) -> TestResult<i64> {
    match change {
        Change::Current => Ok(0),
        Change::Roll => {
            tokio::time::timeout(Duration::from_secs(10), async {
                while chrono::Utc::now() < sub.window_end {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            okapi_ledger::subscriptions::roll(
                &bed.pg,
                &bed.ledger,
                sub,
                chrono::Utc::now(),
                "coexist",
            )
            .await?;
            Ok(2000)
        }
        Change::Cancel | Change::Replace => {
            okapi_ledger::subscriptions::end(&bed.pg, &bed.ledger, sub.id, 3, "coexist").await?;
            if matches!(change, Change::Cancel) {
                return Ok(0);
            }
            let code = format!("coexist-{}", Uuid::new_v4().simple());
            sqlx::query("INSERT INTO plans(plan_code,display_name,grant_micro,kind,period,duration_days) VALUES($1,'Coexist',5000,1,1,30)")
                .bind(&code).execute(&bed.pg).await?;
            let plan = okapi_store::subscriptions::find_sub_plan(&bed.pg, &code)
                .await?
                .ok_or("plan")?;
            okapi_ledger::subscriptions::grant(
                &bed.pg,
                &bed.ledger,
                bed.uid,
                &plan,
                "replacement",
                "test",
            )
            .await?;
            Ok(5000)
        }
    }
}

fn ordinary_bill(
    bed: &Bed,
    id: Uuid,
    source: Option<String>,
) -> TestResult<SettlementInput<'static>> {
    let mut bill = bed.bill(id, 1000)?;
    bill.pool = Pool::Subscription;
    bill.source_window = source;
    bill.delta_micro = -1000;
    bill.event_type = "commit";
    bill.balance_after = None;
    Ok(bill)
}

async fn assert_bills_and_refunds(
    bed: &Bed,
    ids: [Uuid; 2],
    source: Option<&str>,
    change: Change,
    quota: i64,
) -> TestResult {
    let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE id=$1")
        .bind(bed.kid)
        .fetch_one(&bed.pg)
        .await?;
    assert_eq!(used, 2000);
    for id in ids {
        let row: (i16,i64,i64,i64,Option<i64>,Option<String>) = sqlx::query_as("SELECT pool,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,source_window FROM billing_records WHERE request_id=$1 AND status=20")
            .bind(id).fetch_one(&bed.pg).await?;
        assert_eq!(
            row,
            (1, 1000, 1000, 0, Some(600), source.map(str::to_owned))
        );
        let (refund, receipt) =
            okapi_ledger::operations::refund(&bed.pg, &bed.ledger, id, "coexist", "test")
                .await?
                .ok_or("refund")?;
        assert_eq!(refund.amount.as_micros(), 1000);
        assert_eq!(
            refund.credit.as_micros(),
            if matches!(change, Change::Current) {
                1000
            } else {
                0
            }
        );
        assert!(receipt.balance_after.is_some());
        assert!(
            okapi_ledger::operations::refund(&bed.pg, &bed.ledger, id, "coexist", "test")
                .await?
                .is_none()
        );
    }
    let expected = if matches!(change, Change::Current) {
        2000
    } else {
        quota
    };
    bed.repair().await?;
    assert_eq!(bed.event_total(1).await?, expected);
    assert_eq!(bed.sub().await?, expected);
    assert_eq!(bed.wallet().await?, 10000);
    let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE id=$1")
        .bind(bed.kid)
        .fetch_one(&bed.pg)
        .await?;
    assert_eq!(used, 0);
    Ok(())
}

async fn coexist(change: Change, cold: bool, hold_first: bool) -> TestResult {
    let bed = Bed::new().await?;
    let sub = window(&bed, change).await?;
    let hold_id = Uuid::new_v4();
    let hold = bed.reserve(hold_id).await?;
    let regular_id = Uuid::new_v4();
    let mut request = bed.regular(regular_id);
    request.est = Money::from_micros(500);
    let reserved = bed.ledger.reserve(request, chrono::Utc::now()).await?;
    let ReserveOutcome::Reserved {
        pool,
        source_window,
        ..
    } = reserved
    else {
        return Err("ordinary admission".into());
    };
    assert_eq!(pool, Pool::Subscription);
    assert!(source_window.is_some());
    assert_eq!(source_window, hold.source_window);
    assert_eq!(bed.sub().await?, 0);
    let quota = change_window(&bed, &sub, change).await?;
    if !matches!(change, Change::Current) {
        assert_eq!(
            bed.event_total(1).await?,
            quota + 1500,
            "only the durable hold survives a transition"
        );
        assert_eq!(bed.sub().await?, quota);
    }
    if cold {
        bed.redis
            .del::<(), _>(vec![bed.balance_key(), bed.receipt_key(hold_id)])
            .await?;
        bed.repair().await?;
    }
    let ordinary = ordinary_bill(&bed, regular_id, source_window.clone())?;
    if hold_first {
        holds::settle(
            &bed.pg,
            &bed.ledger,
            bed.bill(hold_id, 1000)?,
            chrono::Utc::now(),
        )
        .await?;
        okapi_ledger::sync::record(&bed.pg, &bed.ledger, ordinary.clone()).await?;
    } else {
        okapi_ledger::sync::record(&bed.pg, &bed.ledger, ordinary.clone()).await?;
        holds::settle(
            &bed.pg,
            &bed.ledger,
            bed.bill(hold_id, 1000)?,
            chrono::Utc::now(),
        )
        .await?;
    }
    let closed = bed.row(hold_id).await?;
    assert_eq!(closed.status, Status::Closed);
    assert_eq!(
        closed.credit_micro,
        Some(if matches!(change, Change::Current) {
            500
        } else {
            0
        })
    );
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    assert_eq!(bed.event_total(1).await?, quota);
    assert_eq!(bed.sub().await?, quota);
    bed.repair().await?;
    assert_eq!(bed.sub().await?, quota);
    assert_eq!(bed.wallet().await?, 10000);
    assert!(!okapi_ledger::sync::record(&bed.pg, &bed.ledger, ordinary).await?);
    holds::settle(
        &bed.pg,
        &bed.ledger,
        bed.bill(hold_id, 1000)?,
        chrono::Utc::now(),
    )
    .await?;
    assert_eq!(bed.event_total(1).await?, quota);
    assert_bills_and_refunds(
        &bed,
        [hold_id, regular_id],
        source_window.as_deref(),
        change,
        quota,
    )
    .await
}

#[tokio::test]
async fn ordinary_and_durable_current_period_charge_and_refund_once_in_either_order() -> TestResult
{
    for first in [false, true] {
        coexist(Change::Current, false, first).await?;
    }
    Ok(())
}
#[tokio::test]
async fn ordinary_and_durable_roll_keep_only_durable_funds_in_either_order() -> TestResult {
    for first in [false, true] {
        coexist(Change::Roll, false, first).await?;
    }
    Ok(())
}
#[tokio::test]
async fn ordinary_and_durable_cancellation_leave_no_refundable_quota() -> TestResult {
    for first in [false, true] {
        coexist(Change::Cancel, false, first).await?;
    }
    Ok(())
}
#[tokio::test]
async fn ordinary_and_durable_replacement_cannot_transfer_old_refunds() -> TestResult {
    for first in [false, true] {
        coexist(Change::Replace, false, first).await?;
    }
    Ok(())
}
#[tokio::test]
async fn lost_receipts_after_roll_restore_hold_without_restoring_old_ordinary_quota() -> TestResult
{
    for first in [false, true] {
        coexist(Change::Roll, true, first).await?;
    }
    Ok(())
}
#[tokio::test]
async fn lost_receipts_after_cancellation_cannot_recreate_expired_credit() -> TestResult {
    for first in [false, true] {
        coexist(Change::Cancel, true, first).await?;
    }
    Ok(())
}
#[tokio::test]
async fn lost_receipts_after_replacement_keep_both_charges_in_original_period() -> TestResult {
    for first in [false, true] {
        coexist(Change::Replace, true, first).await?;
    }
    Ok(())
}
