use super::*;
use jobs::{archive, cleanup};

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
async fn archive_lease_protects_reads_through_deletion_but_never_authorizes_new_downloads()
-> TestResult {
    let f = Fixture::new().await?;
    let row = completed(&f).await?;
    assert!(
        archive::open(&f.bed.pg, row.id, row.user_id, row.api_key_id + 1, true)
            .await?
            .is_none()
    );
    let head = archive::open(&f.bed.pg, row.id, row.user_id, row.api_key_id, false)
        .await?
        .ok_or("head")?;
    assert!(head.lease.is_none());
    assert!(head.batch.downloaded_at.is_none());
    let opened = archive::open(&f.bed.pg, row.id, row.user_id, row.api_key_id, true)
        .await?
        .ok_or("open")?;
    let lease = opened.lease.ok_or("lease")?;
    jobs::request_delete(&f.bed.pg, row.id, row.user_id, row.api_key_id).await?;
    assert!(
        archive::open(&f.bed.pg, row.id, row.user_id, row.api_key_id, true)
            .await?
            .is_none()
    );
    assert!(cleanup::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    for slot in 0..3 {
        assert!(archive::content(&f.bed.pg, lease, slot).await?.is_some());
    }
    archive::release(&f.bed.pg, lease).await?;
    archive::release(&f.bed.pg, lease).await?;
    assert!(archive::content(&f.bed.pg, lease, 0).await?.is_none());
    let claim = cleanup::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("cleanup")?;
    cleanup::job_removed(&f.bed.pg, claim.lease).await?;
    cleanup::finish(&f.bed.pg, claim.lease).await?;
    Ok(())
}
#[tokio::test]
async fn abandoned_archive_leases_are_bounded_and_expire_without_blocking_cleanup() -> TestResult {
    let f = Fixture::new().await?;
    let row = completed(&f).await?;
    let mut leases = Vec::new();
    for _ in 0..16 {
        leases.push(
            archive::open(&f.bed.pg, row.id, row.user_id, row.api_key_id, true)
                .await?
                .ok_or("open")?
                .lease
                .ok_or("lease")?,
        );
    }
    assert!(matches!(
        archive::open(&f.bed.pg, row.id, row.user_id, row.api_key_id, true).await,
        Err(jobs::Error::Capacity)
    ));
    sqlx::query("UPDATE image_batches SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(row.id)
        .execute(&f.bed.pg)
        .await?;
    assert!(cleanup::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    sqlx::query(
        "UPDATE image_batch_downloads SET expires_at=now()-interval '1 second' WHERE batch_id=$1",
    )
    .bind(row.id)
    .execute(&f.bed.pg)
    .await?;
    assert!(archive::content(&f.bed.pg, leases[0], 0).await?.is_none());
    let claim = cleanup::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("cleanup")?;
    cleanup::job_removed(&f.bed.pg, claim.lease).await?;
    cleanup::finish(&f.bed.pg, claim.lease).await?;
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM image_batch_downloads WHERE batch_id=$1")
            .bind(row.id)
            .fetch_one(&f.bed.pg)
            .await?;
    assert_eq!(count, 0);
    Ok(())
}
