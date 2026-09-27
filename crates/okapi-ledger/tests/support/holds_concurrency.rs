use super::*;
use fred::interfaces::{HashesInterface, KeysInterface};

async fn limit(bed: &Bed) -> TestResult {
    sqlx::query("UPDATE api_keys SET max_concurrency=1 WHERE id=$1")
        .bind(bed.kid)
        .execute(&bed.pg)
        .await?;
    Ok(())
}
fn ordinary(bed: &Bed) -> okapi_ledger::ReserveRequest {
    let mut request = bed.regular(Uuid::new_v4());
    request.caps.concurrency = 1;
    request
}
async fn denied(bed: &Bed) -> TestResult {
    let snapshot = bed.snapshot().await?;
    assert!(
        matches!(bed.ledger.reserve(ordinary(bed),bed.now).await?,ReserveOutcome::RateLimited{which} if which=="concurrency")
    );
    assert_eq!(bed.snapshot().await?, snapshot);
    Ok(())
}
async fn cancel(bed: &Bed, id: Uuid) -> TestResult {
    let mut bill = bed.bill(id, 0)?;
    bill.state = BillingState::Refunded;
    for _ in 0..2 {
        holds::settle(&bed.pg, &bed.ledger, bill.clone(), bed.now).await?;
    }
    Ok(())
}

#[tokio::test]
async fn durable_and_ordinary_admissions_atomically_compete_for_one_slot() -> TestResult {
    let bed = Bed::new().await?;
    limit(&bed).await?;
    for _ in 0..8 {
        let id = Uuid::new_v4();
        let regular = ordinary(&bed);
        let (normal, durable) = tokio::join!(
            bed.ledger.reserve(regular, bed.now),
            holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 1_500), bed.now)
        );
        match (normal?, durable?) {
            (ReserveOutcome::Reserved { .. }, Admission::ConcurrencyLimited) => {
                assert_eq!(bed.wallet().await?, 9_000);
                assert_eq!(bed.row(id).await?.status, Status::Pending);
                bed.ledger
                    .refund(bed.uid, bed.kid, regular.request_id)
                    .await?;
                bed.reserve(id).await?;
                denied(&bed).await?;
                cancel(&bed, id).await?;
            }
            (ReserveOutcome::RateLimited { which }, Admission::Held { .. }) => {
                assert_eq!(which, "concurrency");
                assert_eq!(bed.wallet().await?, 8_500);
                cancel(&bed, id).await?;
                assert!(matches!(
                    bed.ledger.reserve(regular, bed.now).await?,
                    ReserveOutcome::Reserved { .. }
                ));
                bed.ledger
                    .refund(bed.uid, bed.kid, regular.request_id)
                    .await?;
            }
            other => return Err(format!("two admissions must not pass: {other:?}").into()),
        }
        assert_eq!(bed.wallet().await?, 10_000);
    }
    Ok(())
}

#[tokio::test]
async fn legacy_missing_index_and_zero_cost_holds_still_occupy_and_release_once() -> TestResult {
    let bed = Bed::new().await?;
    limit(&bed).await?;
    let id = Uuid::new_v4();
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 0), bed.now).await?,
        Admission::Held { .. }
    ));
    let _: i64 = bed
        .redis
        .hdel(bed.balance_key(), format!("hc:{}", bed.kid))
        .await?;
    denied(&bed).await?;
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(id, 0), bed.now).await?,
        Admission::Held { replayed: true, .. }
    ));
    let next = Uuid::new_v4();
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(next, 1_500), bed.now).await?,
        Admission::ConcurrencyLimited
    ));
    assert_eq!(bed.wallet().await?, 10_000);
    cancel(&bed, id).await?;
    bed.reserve(next).await?;
    denied(&bed).await?;
    cancel(&bed, next).await?;
    let regular = ordinary(&bed);
    assert!(matches!(
        bed.ledger.reserve(regular, bed.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    bed.ledger
        .refund(bed.uid, bed.kid, regular.request_id)
        .await?;
    assert_eq!(bed.wallet().await?, 10_000);
    Ok(())
}

#[tokio::test]
async fn repair_rebuilds_missing_and_invalid_indexes_without_rebilling() -> TestResult {
    let bed = Bed::new().await?;
    limit(&bed).await?;
    let id = Uuid::new_v4();
    bed.reserve(id).await?;
    for corrupt in ["bad", "01", "-1", "129"] {
        bed.redis
            .hset::<(), _, _>(bed.balance_key(), (format!("hc:{}", bed.kid), corrupt))
            .await?;
        let before = bed.snapshot().await?;
        assert!(matches!(
            bed.ledger.reserve(ordinary(&bed), bed.now).await,
            Err(LedgerError::HoldRecoveryRequired)
        ));
        assert_eq!(bed.snapshot().await?, before);
        bed.repair().await?;
        denied(&bed).await?;
        assert_eq!(bed.wallet().await?, 8_500);
    }
    let _: i64 = bed
        .redis
        .del(vec![bed.balance_key(), bed.receipt_key(id)])
        .await?;
    bed.repair().await?;
    denied(&bed).await?;
    assert_eq!(bed.wallet().await?, 8_500);
    holds::settle(&bed.pg, &bed.ledger, bed.bill(id, 1_000)?, bed.now).await?;
    bed.repair().await?;
    let regular = ordinary(&bed);
    assert!(matches!(
        bed.ledger.reserve(regular, bed.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    bed.ledger
        .refund(bed.uid, bed.kid, regular.request_id)
        .await?;
    assert_eq!(bed.wallet().await?, 9_000);
    bed.evidence(id, 1_000).await
}

#[tokio::test]
async fn durable_slots_are_key_scoped_and_lowered_caps_do_not_revoke_existing_holds() -> TestResult
{
    let bed = Bed::new().await?;
    let mut other = bed.clone();
    other.kid = okapi_store::provision::create_api_key(
        &bed.pg,
        bed.uid,
        &Uuid::new_v4().simple().to_string().repeat(2),
        "other",
    )
    .await?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    bed.reserve(first).await?;
    bed.reserve(second).await?;
    limit(&bed).await?;
    denied(&bed).await?;
    assert!(matches!(
        holds::reserve(&bed.pg, &bed.ledger, bed.request(first, 1_500), bed.now).await?,
        Admission::Held { replayed: true, .. }
    ));
    let regular = ordinary(&other);
    assert!(matches!(
        other.ledger.reserve(regular, other.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    other
        .ledger
        .refund(other.uid, other.kid, regular.request_id)
        .await?;
    cancel(&bed, first).await?;
    denied(&bed).await?;
    cancel(&bed, second).await?;
    assert_eq!(bed.wallet().await?, 10_000);
    Ok(())
}
