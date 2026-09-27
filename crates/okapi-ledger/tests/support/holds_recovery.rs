use super::support::{Bed, TestResult};
use fred::interfaces::{HashesInterface, KeysInterface, LuaInterface};
use okapi_domain::Money;
use okapi_ledger::{
    LedgerError,
    holds::{self, Admission, Status},
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn redis_loss_requires_pg_repair_instead_of_double_reserving() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let _: i64 = bed
        .redis
        .del(vec![bed.balance_key(), bed.receipt_key(id)])
        .await?;
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_500), bed.now).await,
        Err(LedgerError::HoldRecoveryRequired)
    ));
    assert_eq!(bed.wallet().await?, 0);
    let fixed = bed.repair().await?;
    assert_eq!(fixed.wallet.after.as_micros(), 8_500);
    assert_eq!(fixed.wallet.inflight.as_micros(), 1_500);
    assert_eq!(bed.row(id).await?.status, Status::Held);
    bed.reserve(id).await?;
    bed.repair().await?;
    assert_eq!(bed.wallet().await?, 8_500);
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn repair_preserves_regular_reservations_and_restores_missing_hold_receipts() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let regular = bed.regular(Uuid::new_v4());
    bed.ledger.reserve(regular, bed.now).await?;
    let _: i64 = bed.redis.del(bed.receipt_key(id)).await?;
    let _: i64 = bed.redis.hdel(bed.balance_key(), format!("h:{id}")).await?;
    let _: i64 = bed.redis.hset(bed.balance_key(), ("avail", "1")).await?;
    let repaired = bed.repair().await?;
    assert_eq!(repaired.wallet.inflight.as_micros(), 2_500);
    assert_eq!(bed.wallet().await?, 7_500);
    let before = bed.snapshot().await?;
    bed.repair().await?;
    assert_eq!(bed.snapshot().await?, before);
    let ordinary = bed.ledger.list_reservations(bed.uid).await?;
    assert_eq!(ordinary.len(), 1);
    assert_eq!(ordinary[0].request_id, regular.request_id);
    bed.ledger
        .refund(bed.uid, bed.kid, regular.request_id)
        .await?;
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(bed.wallet().await?, 9_000);
    Ok(())
}

#[tokio::test]
async fn unknown_or_malformed_active_hold_blocks_repair_without_writes() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let good: String = bed.redis.get(bed.receipt_key(id)).await?;
    let unknown = format!("h:{}", Uuid::new_v4());
    for invalid in ["{", good.as_str()] {
        let _: i64 = bed
            .redis
            .hset(bed.balance_key(), (&unknown, invalid))
            .await?;
        let before = bed.snapshot().await?;
        assert!(bed.repair().await.is_err());
        assert_eq!(bed.snapshot().await?, before);
        let after: String = bed.redis.get(bed.receipt_key(id)).await?;
        assert_eq!(after, good);
    }
    let _: i64 = bed.redis.hdel(bed.balance_key(), unknown).await?;
    bed.repair().await?;
    assert_eq!(bed.wallet().await?, 8_500);
    Ok(())
}

/// Fail acknowledgement after Redis admission, retaining the committed PG intent.
#[tokio::test]
async fn failed_pg_hold_ack_reuses_original_hot_receipt() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    let constraint = format!("test_hold_ack_{}", bed.uid);
    // Identifiers contain a fixed prefix and a database-generated positive i64 only.
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE balance_holds ADD CONSTRAINT {constraint} CHECK (user_id <> {} OR state <> 'held') NOT VALID",bed.uid))).execute(&bed.pg).await?;
    let result = holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_500), bed.now).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE balance_holds DROP CONSTRAINT {constraint}"
    )))
    .execute(&bed.pg)
    .await?;
    assert!(matches!(result, Err(LedgerError::Sqlx(_))));
    assert_eq!(bed.row(id).await?.status, Status::Pending);
    assert_eq!(bed.wallet().await?, 8_500);
    bed.repair().await?;
    assert_eq!(bed.wallet().await?, 8_500);
    bed.reserve(id).await?;
    assert_eq!(bed.wallet().await?, 8_500);
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await
}

/// Fail PG's closed marker after its bill and Redis credit have both succeeded.
#[tokio::test]
async fn failed_closed_ack_replays_without_second_refund() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let constraint = format!("test_close_ack_{}", bed.uid);
    // Identifiers contain a fixed prefix and a database-generated positive i64 only.
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE balance_holds ADD CONSTRAINT {constraint} CHECK (user_id <> {} OR state <> 'closed') NOT VALID",bed.uid))).execute(&bed.pg).await?;
    let result = holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE balance_holds DROP CONSTRAINT {constraint}"
    )))
    .execute(&bed.pg)
    .await?;
    assert!(result.is_err());
    assert_eq!(bed.row(id).await?.status, Status::Closing);
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await?;
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(bed.row(id).await?.status, Status::Closed);
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn outbox_failure_rolls_back_bill_key_usage_and_closing_state_before_refund() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let constraint = format!("test_hold_outbox_{}", bed.uid);
    // The only interpolated values are a generated numeric suffix and a typed UUID.
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE billing_outbox ADD CONSTRAINT {constraint} CHECK (payload->>'request_id' <> '{id}') NOT VALID"))).execute(&bed.pg).await?;
    let result = holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_outbox DROP CONSTRAINT {constraint}"
    )))
    .execute(&bed.pg)
    .await?;
    assert!(result.is_err());
    assert_eq!(bed.row(id).await?.status, Status::Held);
    assert_eq!(bed.wallet().await?, 8_500);
    assert_eq!(bed.event_total(0).await?, 10_000);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE request_id=$1")
        .bind(id)
        .fetch_one(&bed.pg)
        .await?;
    assert_eq!(rows, 0);
    let used: i64 = sqlx::query_scalar("SELECT used_micro FROM api_keys WHERE id=$1")
        .bind(bed.kid)
        .fetch_one(&bed.pg)
        .await?;
    assert_eq!(used, 0);
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn maximum_exact_lua_integer_survives_reserve_repair_and_refund() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    let max = holds::MAXIMUM_MICROS;
    let topup = Money::from_micros(max - 10_000);
    // PG and Redis initial values are exact integer strings, without a JSON float conversion.
    okapi_ledger::pg::record_credit(&bed.pg, bed.uid, topup, "adjust", "test", json!({})).await?;
    let _: i64 = bed
        .redis
        .hset(bed.balance_key(), ("avail", max.to_string()))
        .await?;
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, max), bed.now).await?,
        Admission::Held { .. }
    ));
    assert_eq!(bed.wallet().await?, 0);
    bed.repair().await?;
    assert_eq!(bed.wallet().await?, 0);
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 0)?, bed.now).await?;
    assert_eq!(bed.wallet().await?, max);
    assert_eq!(bed.event_total(0).await?, max);
    assert!(matches!(
        holds::reserve(
            &bed.pg,
            &bed.ledger,
            bed.request(Uuid::new_v4(), max + 1),
            bed.now
        )
        .await,
        Err(LedgerError::InvalidHold(_))
    ));
    assert_eq!(bed.wallet().await?, max);
    Ok(())
}

#[tokio::test]
async fn cancelling_an_insufficient_pending_hold_never_credits_unfrozen_money() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 20_000), bed.now).await?,
        Admission::Insufficient { .. }
    ));
    let mut bill = bed.bill(id, 0)?;
    bill.state = okapi_domain::BillingState::Refunded;
    for _ in 0..2 {
        let closed = holds::settle(&bed.pg, &bed.ledger, bill.clone(), bed.now).await?;
        assert_eq!(closed.status, Status::Closed);
        assert_eq!(closed.credit_micro, Some(0));
    }
    assert_eq!(bed.wallet().await?, 10_000);
    assert_eq!(bed.event_total(0).await?, 10_000);
    assert!(bed.row(id).await?.cancel_requested);
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 20_000), bed.now).await?,
        Admission::Closed(_)
    ));
    // Exercise the actual old Lua command as if it arrived after cancellation.
    let reply: String = bed
        .redis
        .eval(
            concat!(
                include_str!("../../src/lua/hold_concurrency.lua"),
                "\n",
                include_str!("../../src/lua/hold_reserve.lua")
            ),
            vec![bed.balance_key(), bed.receipt_key(id)],
            vec![
                id.to_string(),
                "20000".into(),
                bed.kid.to_string(),
                super::support::PROOF.into(),
                bed.now.timestamp().to_string(),
            ],
        )
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&reply)?["error"],
        "recovery_required"
    );
    assert_eq!(bed.wallet().await?, 10_000);
    bed.evidence(id, 0).await
}

#[tokio::test]
async fn cancelling_uncertain_admission_refunds_its_existing_hold() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    // PG acknowledgement failure is scoped to this generated user ID.
    let constraint = format!("test_cancel_ack_{}", bed.uid);
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE balance_holds ADD CONSTRAINT {constraint} CHECK (user_id <> {} OR state <> 'held') NOT VALID",bed.uid))).execute(&bed.pg).await?;
    let result = holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_500), bed.now).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE balance_holds DROP CONSTRAINT {constraint}"
    )))
    .execute(&bed.pg)
    .await?;
    assert!(result.is_err());
    assert_eq!(bed.wallet().await?, 8_500);
    let mut bill = bed.bill(id, 0)?;
    bill.state = okapi_domain::BillingState::Failed;
    let closed = holds::settle(&bed.pg, &bed.ledger, bill.clone(), bed.now).await?;
    assert_eq!(closed.credit_micro, Some(1_500));
    assert_eq!(bed.wallet().await?, 10_000);
    holds::settle(&bed.pg, &bed.ledger, bill, bed.now).await?;
    assert_eq!(bed.wallet().await?, 10_000);
    bed.evidence(id, 0).await
}

#[tokio::test]
async fn interrupted_pending_cancellation_stays_fenced_after_redis_loss() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 20_000), bed.now).await?;
    let constraint = format!("test_cancel_bill_{}", bed.uid);
    // This generated UUID is the only interpolated value inside the check expression.
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE billing_outbox ADD CONSTRAINT {constraint} CHECK (payload->>'request_id' <> '{id}') NOT VALID"))).execute(&bed.pg).await?;
    let mut bill = bed.bill(id, 0)?;
    bill.state = okapi_domain::BillingState::Refunded;
    let result = holds::settle(&bed.pg, &bed.ledger, bill.clone(), bed.now).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_outbox DROP CONSTRAINT {constraint}"
    )))
    .execute(&bed.pg)
    .await?;
    assert!(result.is_err());
    assert_eq!(bed.row(id).await?.status, Status::Pending);
    assert!(bed.row(id).await?.cancel_requested);
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 20_000), bed.now).await,
        Err(LedgerError::HoldRecoveryRequired)
    ));
    let _: i64 = bed
        .redis
        .del(vec![bed.balance_key(), bed.receipt_key(id)])
        .await?;
    bed.repair().await?;
    assert_eq!(bed.wallet().await?, 10_000);
    holds::settle(&bed.pg, &bed.ledger, bill, bed.now).await?;
    assert_eq!(bed.wallet().await?, 10_000);
    bed.evidence(id, 0).await
}

#[tokio::test]
async fn subscription_hold_recovers_its_window_after_cold_redis_loss() -> TestResult {
    let bed = Bed::new().await?;
    bed.subscription().await?;
    let id = Uuid::new_v4();
    let original = bed.reserve(id).await?;
    let _: i64 = bed
        .redis
        .del(vec![bed.balance_key(), bed.receipt_key(id)])
        .await?;
    bed.repair().await?;
    assert_eq!(bed.wallet().await?, 10_000);
    assert_eq!(bed.sub().await?, 500);
    let restored = bed.reserve(id).await?;
    assert_eq!(restored.source_window, original.source_window);
    holds::settle(
        &bed.pg,
        &bed.ledger,
        bed.bill(id, 1_000)?,
        chrono::Utc::now(),
    )
    .await?;
    assert_eq!(bed.sub().await?, 1_000);
    assert_eq!(bed.event_total(1).await?, 1_000);
    Ok(())
}

#[tokio::test]
async fn legacy_window_metadata_is_backfilled_only_from_matching_pg_subscription() -> TestResult {
    let mut bed = Bed::new().await?;
    bed.subscription().await?;
    bed.now = chrono::Utc::now();
    let _: i64 = bed.redis.hdel(bed.balance_key(), "sub_epoch").await?;
    let id = Uuid::new_v4();
    let admitted = bed.reserve(id).await?;
    assert_eq!(admitted.pool, Some(1));
    assert!(admitted.source_window.is_some());
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert_eq!(bed.sub().await?, 1_000);
    let _: i64 = bed.redis.hdel(bed.balance_key(), "sub_epoch").await?;
    let _: i64 = bed.redis.hincrby(bed.balance_key(), "sub_until", 1).await?;
    let before = bed.snapshot().await?;
    assert!(matches!(
        holds::reserve(
            &bed.pg,
            &bed.ledger,
            bed.request(Uuid::new_v4(), 1_500),
            bed.now
        )
        .await,
        Err(LedgerError::HoldRecoveryRequired)
    ));
    assert_eq!(bed.snapshot().await?, before);
    bed.repair().await?;
    Ok(())
}

#[tokio::test]
async fn contradictory_receipt_pool_is_rejected_before_repair_writes() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let original: String = bed.redis.get(bed.receipt_key(id)).await?;
    let mut wrong: serde_json::Value = serde_json::from_str(&original)?;
    wrong["pool"] = 1.into();
    wrong["epoch"] = "unrelated-window".into();
    let wrong = wrong.to_string();
    let _: () = bed
        .redis
        .set(bed.receipt_key(id), &wrong, None, None, false)
        .await?;
    let before = bed.snapshot().await?;
    assert!(bed.repair().await.is_err());
    assert_eq!(bed.snapshot().await?, before);
    let _: () = bed
        .redis
        .set(bed.receipt_key(id), &original, None, None, false)
        .await?;
    bed.repair().await?;
    Ok(())
}

#[tokio::test]
async fn out_of_range_wallet_cannot_enter_the_hold_state_machine() -> TestResult {
    let bed = Bed::new().await?;
    let id = Uuid::new_v4();
    let invalid = holds::MAXIMUM_MICROS.checked_add(2).ok_or("test amount")?;
    let _: i64 = bed
        .redis
        .hset(bed.balance_key(), ("avail", invalid.to_string()))
        .await?;
    let before = bed.snapshot().await?;
    let result = holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1), bed.now).await;
    let after = bed.snapshot().await?;
    // Restore this test's injected corruption even when running against the unfixed code.
    let _: i64 = bed
        .redis
        .hset(bed.balance_key(), ("avail", "10000"))
        .await?;
    bed.repair().await?;
    assert!(
        matches!(result, Err(LedgerError::InvalidHold(_))),
        "{result:?}"
    );
    assert_eq!(
        after, before,
        "must reject before writing a hold or debiting"
    );
    Ok(())
}

#[tokio::test]
async fn out_of_range_negative_subscription_cannot_be_hidden_by_a_refund() -> TestResult {
    let bed = Bed::new().await?;
    bed.subscription().await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    let invalid = holds::MAXIMUM_MICROS
        .checked_add(2)
        .and_then(i64::checked_neg)
        .ok_or("test amount")?;
    let _: i64 = bed
        .redis
        .hset(bed.balance_key(), ("sub", invalid.to_string()))
        .await?;
    let before = bed.snapshot().await?;
    let result = holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_499)?, bed.now).await;
    let after = bed.snapshot().await?;
    // PG's committed bill remains authoritative; recovery restores a usable 501 micro quota.
    let _: i64 = bed.redis.hset(bed.balance_key(), ("sub", "0")).await?;
    bed.repair().await?;
    assert_eq!(bed.sub().await?, 501);
    assert!(
        matches!(result, Err(LedgerError::InvalidHold(_))),
        "{result:?}"
    );
    assert_eq!(
        after, before,
        "must not acknowledge or credit an invalid balance"
    );
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_499)?, bed.now).await?;
    assert_eq!(bed.sub().await?, 501);
    bed.evidence(id, 1_499).await
}
