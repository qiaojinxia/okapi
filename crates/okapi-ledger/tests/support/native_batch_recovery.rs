use super::*;

async fn uncertain(f: &Fixture) -> TestResult<(Batch, Lease)> {
    let (_, lease) = f.preparing().await?;
    let row = jobs::mark_submitting(&f.bed.pg, lease).await?;
    jobs::release(&f.bed.pg, lease, 0, None).await?;
    let claim = jobs::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("not claimed")?;
    assert_eq!(claim.batch.state, State::Uncertain);
    Ok((claim.batch, claim.lease))
}
fn observation() -> Observation<'static> {
    static EMPTY: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));
    Observation {
        job_name: "batches/job",
        state: RemoteState::Running,
        output_ref: &EMPTY,
    }
}

#[tokio::test]
async fn recovery_checkpoints_resume_but_expired_executors_cannot_adopt() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = uncertain(&f).await?;
    let first = jobs::recovery(&f.bed.pg, lease).await?;
    let scan = jobs::recovery_page(
        &f.bed.pg,
        lease,
        &first,
        Some("page-a"),
        &["batches/job".into()],
        false,
    )
    .await?;
    assert_eq!(scan.pages, 1);
    assert!(!scan.complete);
    assert!(
        jobs::adopt_recovered(&f.bed.pg, lease, observation())
            .await
            .is_err()
    );
    assert!(
        jobs::recovery_page(&f.bed.pg, lease, &first, None, &[], false)
            .await
            .is_err()
    );
    f.expire_lease(row.id).await?;
    let next = jobs::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("not reclaimed")?;
    let scan = jobs::recovery(&f.bed.pg, next.lease).await?;
    assert_eq!(scan.next_page.as_deref(), Some("page-a"));
    assert!(
        jobs::recovery_page(&f.bed.pg, lease, &scan, None, &[], false)
            .await
            .is_err()
    );
    let scan = jobs::recovery_page(
        &f.bed.pg,
        next.lease,
        &scan,
        None,
        &["batches/job".into()],
        false,
    )
    .await?;
    assert!(scan.complete && !scan.conflict);
    assert!(
        jobs::adopt_recovered(&f.bed.pg, lease, observation())
            .await
            .is_err()
    );
    let recovered = jobs::adopt_recovered(&f.bed.pg, next.lease, observation()).await?;
    assert_eq!(recovered.state, State::Running);
    assert_eq!(recovered.provider_job_name.as_deref(), Some("batches/job"));
    assert!(jobs::recovery(&f.bed.pg, next.lease).await.is_err());
    Ok(())
}

#[tokio::test]
async fn recovery_restart_keeps_prior_candidates_and_ambiguity_is_durable() -> TestResult {
    let f = Fixture::new().await?;
    let (_, lease) = uncertain(&f).await?;
    let first = jobs::recovery(&f.bed.pg, lease).await?;
    jobs::recovery_page(
        &f.bed.pg,
        lease,
        &first,
        Some("expired"),
        &["batches/job".into()],
        false,
    )
    .await?;
    jobs::restart_recovery(&f.bed.pg, lease).await?;
    let scan = jobs::recovery(&f.bed.pg, lease).await?;
    assert_eq!(scan.pages, 0);
    assert_eq!(scan.candidate_name.as_deref(), Some("batches/job"));
    let scan = jobs::recovery_page(
        &f.bed.pg,
        lease,
        &scan,
        None,
        &["batches/another".into()],
        false,
    )
    .await?;
    assert!(scan.conflict);
    jobs::restart_recovery(&f.bed.pg, lease).await?;
    assert!(jobs::recovery(&f.bed.pg, lease).await?.conflict);
    assert!(
        jobs::adopt_recovered(&f.bed.pg, lease, observation())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn recovery_detects_cross_page_cycles_and_concurrent_checkpoint_writers() -> TestResult {
    let f = Fixture::new().await?;
    let (_, lease) = uncertain(&f).await?;
    let scan = jobs::recovery(&f.bed.pg, lease).await?;
    let scan = jobs::recovery_page(&f.bed.pg, lease, &scan, Some("page-a"), &[], false).await?;
    let (a, b) = tokio::join!(
        jobs::recovery_page(&f.bed.pg, lease, &scan, Some("page-b"), &[], false),
        jobs::recovery_page(&f.bed.pg, lease, &scan, Some("page-b"), &[], false)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let scan = jobs::recovery(&f.bed.pg, lease).await?;
    assert!(
        jobs::recovery_page(&f.bed.pg, lease, &scan, Some("page-a"), &[], false)
            .await
            .is_err()
    );
    assert_eq!(jobs::recovery(&f.bed.pg, lease).await?.pages, 2);
    assert!(
        jobs::recovery_page(&f.bed.pg, lease, &scan, Some(&"x".repeat(4097)), &[], false)
            .await
            .is_err()
    );
    let scan = jobs::recovery_page(
        &f.bed.pg,
        lease,
        &scan,
        None,
        &["batches/job".into()],
        false,
    )
    .await?;
    assert!(scan.complete);
    jobs::conflict_recovery(&f.bed.pg, lease).await?;
    assert!(
        jobs::adopt_recovered(&f.bed.pg, lease, observation())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn authoritative_late_ack_wins_over_lookup_hints_without_reopening_submission() -> TestResult
{
    let f = Fixture::new().await?;
    let (row, lease) = uncertain(&f).await?;
    let scan = jobs::recovery(&f.bed.pg, lease).await?;
    jobs::recovery_page(
        &f.bed.pg,
        lease,
        &scan,
        None,
        &["batches/hint".into()],
        false,
    )
    .await?;
    assert!(
        jobs::adopt_recovered(&f.bed.pg, lease, observation())
            .await
            .is_err()
    );
    let late = jobs::observe(
        &f.bed.pg,
        row.id,
        row.submit_intent.ok_or("missing intent")?,
        observation(),
    )
    .await?;
    assert_eq!(late.provider_job_name.as_deref(), Some("batches/job"));
    assert!(
        jobs::adopt_recovered(&f.bed.pg, lease, observation())
            .await
            .is_err()
    );
    assert!(jobs::mark_submitting(&f.bed.pg, lease).await.is_err());
    Ok(())
}
