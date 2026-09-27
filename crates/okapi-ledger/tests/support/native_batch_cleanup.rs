use super::*;
use jobs::cleanup;

async fn completed(f: &Fixture) -> TestResult<Batch> {
    let (_, lease) = f.collecting(RemoteState::Succeeded).await?;
    for slot in 0..3 {
        f.success(lease, slot).await?;
    }
    let row = jobs::seal_results(&f.bed.pg, lease).await?;
    f.settle(&row, 3).await?;
    Ok(jobs::finish(&f.bed.pg, lease).await?)
}

#[tokio::test]
async fn cleanup_claim_requires_settled_terminal_and_delete_or_expiry() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = f.collecting(RemoteState::Succeeded).await?;
    assert!(cleanup::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    assert!(
        jobs::request_delete(&f.bed.pg, row.id, row.user_id, row.api_key_id)
            .await
            .is_err()
    );
    for slot in 0..3 {
        f.success(lease, slot).await?;
    }
    let row = jobs::seal_results(&f.bed.pg, lease).await?;
    assert!(cleanup::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    f.settle(&row, 3).await?;
    let row = jobs::finish(&f.bed.pg, lease).await?;
    assert!(cleanup::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    assert!(
        jobs::request_delete(&f.bed.pg, row.id, row.user_id, row.api_key_id + 1)
            .await?
            .is_none()
    );
    sqlx::query("UPDATE image_batches SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(row.id)
        .execute(&f.bed.pg)
        .await?;
    let claim = cleanup::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("not claimed")?;
    assert!(!claim.batch.delete_requested);
    assert!(!claim.progress.job_removed);
    assert!(jobs::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    assert!(cleanup::finish(&f.bed.pg, claim.lease).await.is_err());
    cleanup::release(&f.bed.pg, claim.lease, 0, None).await?;
    sqlx::query("UPDATE image_batches SET actual_micro=0 WHERE id=$1")
        .bind(row.id)
        .execute(&f.bed.pg)
        .await?;
    assert!(matches!(
        cleanup::claim(&f.bed.pg, Some(row.id)).await,
        Err(jobs::Error::NotSettled)
    ));
    Ok(())
}

#[tokio::test]
async fn cleanup_lease_fences_restart_and_purges_only_owned_artifacts() -> TestResult {
    let f = Fixture::new().await?;
    let row = completed(&f).await?;
    let other = f.create().await?;
    jobs::request_delete(&f.bed.pg, row.id, row.user_id, row.api_key_id).await?;
    let first = cleanup::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("claim")?;
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let pg = f.bed.pg.clone();
        calls.spawn(async move { cleanup::claim(&pg, Some(row.id)).await });
    }
    while let Some(r) = calls.join_next().await {
        assert!(r??.is_none());
    }
    cleanup::operation(
        &f.bed.pg,
        first.lease,
        Some("projects/p/locations/l/operations/delete"),
    )
    .await?;
    f.expire_lease(row.id).await?;
    let second = cleanup::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("reclaim")?;
    assert_eq!(
        second.progress.operation.as_deref(),
        Some("projects/p/locations/l/operations/delete")
    );
    assert!(cleanup::renew(&f.bed.pg, first.lease).await.is_err());
    assert!(cleanup::job_removed(&f.bed.pg, first.lease).await.is_err());
    assert!(cleanup::payload(&f.bed.pg, first.lease).await.is_err());
    assert!(cleanup::finish(&f.bed.pg, first.lease).await.is_err());
    cleanup::job_removed(&f.bed.pg, second.lease).await?;
    cleanup::release(&f.bed.pg, second.lease, 0, Some("batch_cleanup_retry")).await?;
    let third = cleanup::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("resume")?;
    assert!(third.progress.job_removed);
    assert!(third.progress.operation.is_none());
    cleanup::finish(&f.bed.pg, third.lease).await?;
    let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT COUNT(*) FROM image_batch_payloads WHERE batch_id=$1),(SELECT COUNT(*) FROM image_batch_outputs WHERE batch_id=$1),(SELECT COUNT(*) FROM image_batch_items WHERE batch_id=$1),(SELECT COUNT(*) FROM image_batch_payloads WHERE batch_id=$2)").bind(row.id).bind(other.id).fetch_one(&f.bed.pg).await?;
    assert_eq!(counts, (0, 0, 0, 1));
    let after: Batch = sqlx::query_as("SELECT * FROM image_batches WHERE id=$1")
        .bind(row.id)
        .fetch_one(&f.bed.pg)
        .await?;
    assert!(after.cleanup_done);
    assert_eq!(after.storage_budget, 512 * 1024);
    assert_eq!(after.actual_micro, row.actual_micro);
    assert_eq!(after.pricing_snapshot, row.pricing_snapshot);
    assert!(cleanup::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn cleanup_releases_artifact_capacity_but_keeps_idempotency_and_metadata_budget() -> TestResult
{
    let f = Fixture::new().await?;
    let row = completed(&f).await?;
    sqlx::query("UPDATE image_batches SET idempotency_hash=$2 WHERE id=$1")
        .bind(row.id)
        .bind(PROOF)
        .execute(&f.bed.pg)
        .await?;
    let limits = || Limits {
        per_user_jobs: 1,
        per_user_bytes: row.storage_budget + 512 * 1024,
        ..Limits::default()
    };
    assert!(matches!(
        jobs::create(&f.bed.pg, f.request(Uuid::new_v4()), limits()).await,
        Err(jobs::Error::Capacity)
    ));
    jobs::request_delete(&f.bed.pg, row.id, row.user_id, row.api_key_id).await?;
    let claim = cleanup::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("claim")?;
    cleanup::job_removed(&f.bed.pg, claim.lease).await?;
    cleanup::finish(&f.bed.pg, claim.lease).await?;
    assert!(matches!(
        jobs::replay(&f.bed.pg, row.user_id, row.api_key_id, PROOF, PROOF).await,
        Err(jobs::Error::IdempotencyConflict)
    ));
    let mut duplicate = f.request(Uuid::new_v4());
    duplicate.idempotency_hash = Some(PROOF);
    assert!(matches!(
        jobs::create(&f.bed.pg, duplicate, limits()).await,
        Err(jobs::Error::IdempotencyConflict)
    ));
    let tight = Limits {
        per_user_bytes: row.storage_budget,
        ..limits()
    };
    assert!(matches!(
        jobs::create(&f.bed.pg, f.request(Uuid::new_v4()), tight).await,
        Err(jobs::Error::Capacity)
    ));
    assert!(matches!(
        jobs::create(&f.bed.pg, f.request(Uuid::new_v4()), limits()).await?,
        Created::New(_)
    ));
    Ok(())
}
