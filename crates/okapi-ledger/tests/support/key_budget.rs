use super::support::{Bed, TestResult};
use okapi_domain::Money;
use okapi_ledger::{LedgerError, ReserveOutcome, holds};
use uuid::Uuid;

async fn cap(bed: &Bed, amount: Option<i64>) -> TestResult {
    sqlx::query("UPDATE api_keys SET quota_mode=$2,quota_micro=$3 WHERE id=$1")
        .bind(bed.kid)
        .bind(i16::from(amount.is_some()))
        .bind(amount)
        .execute(&bed.pg)
        .await?;
    Ok(())
}

#[tokio::test]
async fn concurrent_requests_cannot_reserve_the_same_key_budget() -> TestResult {
    let bed = Bed::new().await?;
    cap(&bed, Some(1_500)).await?;
    let a = bed.regular(Uuid::new_v4());
    let b = bed.regular(Uuid::new_v4());
    let (ra, rb) = tokio::join!(
        bed.ledger.reserve_for_key(&bed.pg, true, a, bed.now),
        bed.ledger.reserve_for_key(&bed.pg, true, b, bed.now)
    );
    let winner = match (ra, rb) {
        (Ok(ReserveOutcome::Reserved { .. }), Err(LedgerError::KeyQuotaExceeded)) => a,
        (Err(LedgerError::KeyQuotaExceeded), Ok(ReserveOutcome::Reserved { .. })) => b,
        other => panic!("expected exactly one admission: {other:?}"),
    };
    assert_eq!(bed.wallet().await?, 9_000);
    bed.ledger
        .refund(bed.uid, bed.kid, winner.request_id)
        .await?;
    assert!(matches!(
        bed.ledger
            .reserve_for_key(&bed.pg, true, bed.regular(Uuid::new_v4()), bed.now)
            .await?,
        ReserveOutcome::Reserved { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn ordinary_and_durable_work_share_the_same_key_budget_in_both_directions() -> TestResult {
    let bed = Bed::new().await?;
    cap(&bed, Some(2_000)).await?;
    let regular = bed.regular(Uuid::new_v4());
    bed.ledger
        .reserve_for_key(&bed.pg, true, regular, bed.now)
        .await?;
    let id = Uuid::new_v4();
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_500), bed.now).await,
        Err(LedgerError::KeyQuotaExceeded)
    ));
    bed.ledger
        .refund(bed.uid, bed.kid, regular.request_id)
        .await?;
    bed.reserve(id).await?;
    assert!(matches!(
        bed.ledger
            .reserve_for_key(&bed.pg, true, bed.regular(Uuid::new_v4()), bed.now)
            .await,
        Err(LedgerError::KeyQuotaExceeded)
    ));
    // Existing durable admissions remain replayable even when their cap is reduced.
    cap(&bed, Some(1_000)).await?;
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_500), bed.now).await?,
        holds::Admission::Held { replayed: true, .. }
    ));
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    assert!(matches!(
        bed.ledger
            .reserve_for_key(&bed.pg, true, bed.regular(Uuid::new_v4()), bed.now)
            .await,
        Err(LedgerError::KeyQuotaExceeded)
    ));
    Ok(())
}

#[tokio::test]
async fn fresh_spending_and_limit_changes_are_used_and_other_keys_stay_independent() -> TestResult {
    let bed = Bed::new().await?;
    cap(&bed, Some(1_500)).await?;
    let request = bed.regular(Uuid::new_v4());
    bed.ledger
        .reserve_for_key(&bed.pg, true, request, bed.now)
        .await?;
    let mut bill = bed.bill(request.request_id, 600)?;
    bill.event_type = "commit";
    bill.delta_micro = -600;
    okapi_ledger::sync::record(&bed.pg, &bed.ledger, bill).await?;
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    assert!(matches!(
        bed.ledger
            .reserve_for_key(&bed.pg, true, bed.regular(Uuid::new_v4()), bed.now)
            .await,
        Err(LedgerError::KeyQuotaExceeded)
    ));
    // A sibling key is not charged for this key's spending or reservations.
    let other = okapi_store::provision::create_api_key(
        &bed.pg,
        bed.uid,
        &Uuid::new_v4().simple().to_string().repeat(2),
        "sk-other",
    )
    .await?;
    let mut sibling = bed.regular(Uuid::new_v4());
    sibling.api_key_id = other;
    bed.ledger
        .reserve_for_key(&bed.pg, false, sibling, bed.now)
        .await?;
    cap(&bed, Some(1_600)).await?;
    let mut exact = bed.regular(Uuid::new_v4());
    exact.est = Money::from_micros(1_000);
    assert!(matches!(
        bed.ledger
            .reserve_for_key(&bed.pg, true, exact, bed.now)
            .await?,
        ReserveOutcome::Reserved { .. }
    ));
    cap(&bed, None).await?;
    // A stale 'limited' hint only adds a fresh PG read; clearing is immediately effective.
    assert!(matches!(
        bed.ledger
            .reserve_for_key(&bed.pg, true, bed.regular(Uuid::new_v4()), bed.now)
            .await?,
        ReserveOutcome::Reserved { .. }
    ));
    Ok(())
}
