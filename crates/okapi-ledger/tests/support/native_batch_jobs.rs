use crate::support::{Bed, PROOF, TestResult};
use okapi_domain::{BillingState, Money};
use okapi_ledger::holds::{self, Admission, FrozenReserve};
use okapi_store::image_batches::{
    self as jobs, Batch, Created, Lease, Limits, NewBatch, NewItem, Observation, Output,
    RemoteState, Staged, State, UnitQuote,
};
use serde_json::{Value, json};
use uuid::Uuid;
#[path = "native_batch_archive.rs"]
mod archive;
#[path = "native_batch_cleanup.rs"]
mod cleanup;
#[path = "native_batch_recovery.rs"]
mod recovery;

struct Fixture {
    bed: Bed,
    channel: i64,
    channel_key: i64,
    pricing: Value,
    quote: Value,
    items: Vec<NewItem<'static>>,
}
impl Fixture {
    async fn new() -> TestResult<Self> {
        let bed = Bed::new().await?;
        let (channel, channel_key) = okapi_store::provision::create_channel(
            &bed.pg,
            &format!("native-job-{}", Uuid::new_v4()),
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta",
            "batch-test-credential",
            &["batch-image"],
            false,
            None,
        )
        .await?;
        let pricing = serde_json::to_value(&bed.pricing)?;
        let quote = serde_json::to_value(UnitQuote {
            amount: 500,
            original: 500,
            discount: 0,
            list_price: 500,
            upstream_cost: Some(300),
        })?;
        Ok(Self {
            bed,
            channel,
            channel_key,
            pricing,
            quote,
            items: vec![
                NewItem {
                    custom_id: "first",
                    prompt_preview: "sample",
                    outputs: 2,
                },
                NewItem {
                    custom_id: "second",
                    prompt_preview: "sample",
                    outputs: 1,
                },
            ],
        })
    }
    fn request(&self, id: Uuid) -> NewBatch<'_> {
        NewBatch {
            id,
            user_id: self.bed.uid,
            api_key_id: self.bed.kid,
            request_hash: PROOF,
            idempotency_hash: None,
            task_name: "batch test",
            parent_id: None,
            model: "batch-image",
            group: "default",
            provider: "gemini",
            channel_id: self.channel,
            channel_key_id: self.channel_key,
            upstream_model: "upstream-image",
            pricing: &self.pricing,
            unit_quote: &self.quote,
            maximum: Money::from_micros(1500),
            input: b"{\"key\":\"first\"}\n",
            binding: b"sealed-account-binding",
            items: &self.items,
            client_ip: None,
            client_type: "test",
        }
    }
    async fn create(&self) -> TestResult<Batch> {
        match jobs::create(
            &self.bed.pg,
            self.request(Uuid::new_v4()),
            Limits::default(),
        )
        .await?
        {
            Created::New(row) => Ok(row),
            Created::Existing(_) => Err("unexpected replay".into()),
        }
    }
    async fn freeze(&self, row: &Batch) -> TestResult<bool> {
        match holds::reserve_frozen(
            &self.bed.pg,
            &self.bed.ledger,
            FrozenReserve {
                id: row.id,
                user_id: row.user_id,
                api_key_id: row.api_key_id,
                model: &row.model_name,
                request_hash: &row.request_hash,
                maximum: Money::from_micros(row.maximum_micro),
                pricing: &row.pricing_snapshot,
            },
            self.bed.now,
        )
        .await?
        {
            Admission::Held { replayed, .. } => Ok(replayed),
            other => Err(format!("unexpected admission {other:?}").into()),
        }
    }
    async fn preparing(&self) -> TestResult<(Batch, Lease)> {
        let row = self.create().await?;
        let claim = jobs::claim(&self.bed.pg, Some(row.id))
            .await?
            .ok_or("not claimed")?;
        self.freeze(&row).await?;
        let row = jobs::prepare(&self.bed.pg, claim.lease).await?;
        Ok((row, claim.lease))
    }
    async fn collecting(&self, state: RemoteState) -> TestResult<(Batch, Lease)> {
        let (_, lease) = self.preparing().await?;
        let row = jobs::mark_submitting(&self.bed.pg, lease).await?;
        let row = jobs::observe(
            &self.bed.pg,
            row.id,
            row.submit_intent.ok_or("missing intent")?,
            Observation {
                job_name: "batches/test",
                state,
                output_ref: &json!({"file":"files/results"}),
            },
        )
        .await?;
        Ok((row, lease))
    }
    async fn success(&self, lease: Lease, slot: u32) -> TestResult<Staged> {
        Ok(jobs::stage(
            &self.bed.pg,
            lease,
            slot,
            Output::Success {
                content: b"\x89PNG\r\n\x1a\nprivate-test-bytes",
                content_type: "image/png",
                usage: &json!({"output_tokens":2}),
            },
        )
        .await?)
    }
    async fn failure(&self, lease: Lease, slot: u32) -> TestResult<Staged> {
        Ok(jobs::stage(
            &self.bed.pg,
            lease,
            slot,
            Output::Failed {
                error_code: "blocked",
                usage: &json!({}),
            },
        )
        .await?)
    }
    async fn settle(&self, row: &Batch, units: u32) -> TestResult {
        let quote: UnitQuote = serde_json::from_value(row.unit_quote.clone())?;
        let total = quote.total(units)?;
        let mut bill = self.bed.bill(row.id, total.amount)?;
        bill.original = Money::from_micros(total.original);
        bill.discount = Money::from_micros(total.discount);
        bill.list_price = Money::from_micros(total.list_price);
        bill.upstream_cost = total.upstream_cost.map(Money::from_micros);
        let mut pricing = row.pricing_snapshot.clone();
        pricing["media_units"] = json!(units);
        bill.pricing_snapshot = Some(pricing);
        if units == 0 {
            bill.state = BillingState::Refunded;
        }
        holds::settle(&self.bed.pg, &self.bed.ledger, bill, self.bed.now).await?;
        Ok(())
    }
    async fn expire_lease(&self, id: Uuid) -> TestResult {
        sqlx::query("UPDATE image_batches SET lease_until=now()-interval '1 second',next_run_at=now() WHERE id=$1").bind(id).execute(&self.bed.pg).await?;
        Ok(())
    }
}

#[tokio::test]
async fn native_admission_is_atomic_and_idempotent_under_concurrency() -> TestResult {
    let f = std::sync::Arc::new(Fixture::new().await?);
    let id = Uuid::new_v4();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let f = f.clone();
        tasks.spawn(async move {
            let mut request = f.request(id);
            request.idempotency_hash = Some(PROOF);
            jobs::create(
                &f.bed.pg,
                request,
                Limits {
                    per_user_active: 1,
                    ..Limits::default()
                },
            )
            .await
        });
    }
    let mut fresh = 0;
    while let Some(result) = tasks.join_next().await {
        match result?? {
            Created::New(row) => {
                fresh += 1;
                assert_eq!(row.id, id);
            }
            Created::Existing(row) => assert_eq!(row.id, id),
        }
    }
    assert_eq!(fresh, 1);
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT COUNT(*) FROM image_batch_payloads WHERE batch_id=$1),(SELECT COUNT(*) FROM image_batch_items WHERE batch_id=$1),(SELECT COUNT(*) FROM image_batch_outputs WHERE batch_id=$1)").bind(id).fetch_one(&f.bed.pg).await?;
    assert_eq!(counts, (1, 2, 3));
    let mut request = f.request(Uuid::new_v4());
    request.idempotency_hash = Some(PROOF);
    request.request_hash = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    assert!(matches!(
        jobs::create(&f.bed.pg, request, Limits::default()).await,
        Err(jobs::Error::IdempotencyConflict)
    ));
    assert_eq!(
        jobs::list(&f.bed.pg, f.bed.uid, f.bed.kid, None, 20)
            .await?
            .data
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn native_capacity_validation_and_wrong_channel_cannot_leave_partial_jobs() -> TestResult {
    let f = Fixture::new().await?;
    let id = Uuid::new_v4();
    assert!(matches!(
        jobs::create(
            &f.bed.pg,
            f.request(id),
            Limits {
                per_user_bytes: 1,
                ..Limits::default()
            }
        )
        .await,
        Err(jobs::Error::Capacity)
    ));
    let mut wrong = f.request(id);
    wrong.provider = "vertex";
    assert!(matches!(
        jobs::create(&f.bed.pg, wrong, Limits::default()).await,
        Err(jobs::Error::AdmissionChanged)
    ));
    let duplicate = [
        NewItem {
            custom_id: "dup",
            prompt_preview: "",
            outputs: 1,
        },
        NewItem {
            custom_id: "dup",
            prompt_preview: "",
            outputs: 2,
        },
    ];
    let mut wrong = f.request(id);
    wrong.items = &duplicate;
    assert!(matches!(
        jobs::create(&f.bed.pg, wrong, Limits::default()).await,
        Err(jobs::Error::Invalid("batch_items"))
    ));
    let mut wrong = f.request(id);
    wrong.maximum = Money::from_micros(1499);
    assert!(matches!(
        jobs::create(&f.bed.pg, wrong, Limits::default()).await,
        Err(jobs::Error::Invalid("batch_quote"))
    ));
    assert!(
        jobs::owned(&f.bed.pg, id, f.bed.uid, f.bed.kid)
            .await?
            .is_none()
    );
    assert_eq!(f.bed.wallet().await?, 10_000);
    Ok(())
}

#[tokio::test]
async fn native_list_pages_twenty_rows_without_cross_key_cursor_or_parent_leak() -> TestResult {
    let f = Fixture::new().await?;
    let limits = Limits {
        per_user_active: 24,
        per_key_active: 24,
        ..Limits::default()
    };
    for _ in 0..23 {
        jobs::create(&f.bed.pg, f.request(Uuid::new_v4()), limits).await?;
    }
    let first = jobs::list(&f.bed.pg, f.bed.uid, f.bed.kid, None, 20).await?;
    assert_eq!(first.data.len(), 20);
    assert!(first.has_more);
    let cursor = first.data.last().ok_or("missing last")?.id;
    let second = jobs::list(&f.bed.pg, f.bed.uid, f.bed.kid, Some(cursor), 20).await?;
    assert_eq!(second.data.len(), 3);
    assert!(!second.has_more);
    assert!(
        second
            .data
            .iter()
            .all(|r| first.data.iter().all(|p| p.id != r.id))
    );
    let kid = okapi_store::provision::create_api_key(
        &f.bed.pg,
        f.bed.uid,
        &Uuid::new_v4().simple().to_string().repeat(2),
        "sk-other",
    )
    .await?;
    assert!(
        jobs::list(&f.bed.pg, f.bed.uid, kid, None, 20)
            .await?
            .data
            .is_empty()
    );
    assert!(
        jobs::list(&f.bed.pg, f.bed.uid, kid, Some(cursor), 20)
            .await
            .is_err()
    );
    assert!(
        jobs::owned(&f.bed.pg, cursor, f.bed.uid, kid)
            .await?
            .is_none()
    );
    assert!(
        jobs::cancel(&f.bed.pg, cursor, f.bed.uid, kid)
            .await?
            .is_none()
    );
    let mut child = f.request(Uuid::new_v4());
    child.api_key_id = kid;
    child.parent_id = Some(cursor);
    assert!(matches!(
        jobs::create(&f.bed.pg, child, limits).await,
        Err(jobs::Error::ParentOwner)
    ));
    for limit in [0, 101, u32::MAX] {
        assert!(
            jobs::list(&f.bed.pg, f.bed.uid, f.bed.kid, None, limit)
                .await
                .is_err()
        );
    }
    Ok(())
}

#[tokio::test]
async fn native_restart_uses_saved_price_and_lease_fences_private_payload() -> TestResult {
    let mut f = Fixture::new().await?;
    let row = f.create().await?;
    let claim = jobs::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("not claimed")?;
    assert!(jobs::prepare(&f.bed.pg, claim.lease).await.is_err());
    f.bed.pricing.epoch = 99;
    f.bed.pricing.per_call_price_usd = Some(Money::from_micros(9000));
    // Admission persists the pending financial intent atomically with the job.
    assert_eq!(f.bed.row(row.id).await?.status, holds::Status::Pending);
    assert!(f.freeze(&row).await?);
    assert!(f.freeze(&row).await?);
    assert_eq!(f.bed.wallet().await?, 8500);
    assert_eq!(
        f.bed.row(row.id).await?.pricing_snapshot,
        row.pricing_snapshot
    );
    jobs::prepare(&f.bed.pg, claim.lease).await?;
    jobs::save_input(
        &f.bed.pg,
        claim.lease,
        &json!({"file":"files/input"}),
        Some(b"sealed-session"),
    )
    .await?;
    let body = jobs::payload(&f.bed.pg, claim.lease).await?;
    assert_eq!(body.binding, b"sealed-account-binding");
    assert_eq!(
        body.upload_session.as_deref(),
        Some(b"sealed-session".as_slice())
    );
    assert_eq!(body.input, f.request(row.id).input);
    f.expire_lease(row.id).await?;
    let newer = jobs::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("not reclaimed")?;
    assert_eq!(newer.batch.state, State::Preparing);
    assert!(jobs::payload(&f.bed.pg, claim.lease).await.is_err());
    assert!(jobs::mark_submitting(&f.bed.pg, claim.lease).await.is_err());
    assert!(
        jobs::release(&f.bed.pg, claim.lease, 0, None)
            .await
            .is_err()
    );
    assert!(jobs::payload(&f.bed.pg, newer.lease).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn native_workers_claim_once_and_expired_submit_is_uncertain_not_retried() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = f.preparing().await?;
    jobs::release(&f.bed.pg, lease, 0, None).await?;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..24 {
        let pg = f.bed.pg.clone();
        let id = row.id;
        tasks.spawn(async move { jobs::claim(&pg, Some(id)).await });
    }
    let mut claims = Vec::new();
    while let Some(result) = tasks.join_next().await {
        if let Some(claim) = result?? {
            claims.push(claim);
        }
    }
    assert_eq!(claims.len(), 1);
    let lease = claims.pop().ok_or("missing claim")?.lease;
    let submitting = jobs::mark_submitting(&f.bed.pg, lease).await?;
    let intent = submitting.submit_intent.ok_or("missing intent")?;
    f.expire_lease(row.id).await?;
    let next = jobs::claim(&f.bed.pg, Some(row.id))
        .await?
        .ok_or("missing recovery")?;
    assert_eq!(next.batch.state, State::Uncertain);
    assert!(jobs::mark_submitting(&f.bed.pg, next.lease).await.is_err());
    assert!(
        jobs::abort_before_submission(&f.bed.pg, next.lease, "timeout")
            .await
            .is_err()
    );
    let observed = jobs::observe(
        &f.bed.pg,
        row.id,
        intent,
        Observation {
            job_name: "batches/late",
            state: RemoteState::Running,
            output_ref: &json!({}),
        },
    )
    .await?;
    assert_eq!(observed.state, State::Running);
    assert!(
        jobs::observe(
            &f.bed.pg,
            row.id,
            intent,
            Observation {
                job_name: "batches/another",
                state: RemoteState::Running,
                output_ref: &json!({})
            }
        )
        .await
        .is_err()
    );
    assert_eq!(f.bed.wallet().await?, 8500);
    Ok(())
}

#[tokio::test]
async fn native_lease_expiring_while_waiting_for_a_row_lock_cannot_submit() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = f.preparing().await?;
    sqlx::query(
        "UPDATE image_batches SET lease_until=clock_timestamp()+interval '1 second' WHERE id=$1",
    )
    .bind(row.id)
    .execute(&f.bed.pg)
    .await?;
    let mut blocker = f.bed.pg.begin().await?;
    sqlx::query("SELECT id FROM image_batches WHERE id=$1 FOR UPDATE")
        .bind(row.id)
        .execute(&mut *blocker)
        .await?;
    let pg = f.bed.pg.clone();
    let task = tokio::spawn(async move { jobs::mark_submitting(&pg, lease).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT * FROM image_batches WHERE id=$1 AND lease_id=$2%')")
                .fetch_one(&f.bed.pg).await?;
            if waiting {break Ok::<_, sqlx::Error>(());}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await??;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let expired: bool = sqlx::query_scalar(
                "SELECT lease_until<clock_timestamp() FROM image_batches WHERE id=$1",
            )
            .bind(row.id)
            .fetch_one(&f.bed.pg)
            .await?;
            if expired {
                break Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    blocker.commit().await?;
    assert!(matches!(task.await?, Err(jobs::Error::LeaseLost)));
    let row = jobs::owned(&f.bed.pg, row.id, f.bed.uid, f.bed.kid)
        .await?
        .ok_or("missing job")?;
    assert_eq!(row.state, State::Preparing);
    assert!(row.submit_intent.is_none());
    Ok(())
}

#[tokio::test]
async fn native_partial_results_are_private_until_exact_closed_settlement() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = f.collecting(RemoteState::PartiallySucceeded).await?;
    assert_eq!(f.success(lease, 0).await?, Staged::New);
    assert_eq!(f.success(lease, 0).await?, Staged::Replay);
    assert!(
        jobs::content_owned(&f.bed.pg, row.id, f.bed.uid, f.bed.kid, 0)
            .await?
            .is_none()
    );
    assert!(jobs::seal_results(&f.bed.pg, lease).await.is_err());
    f.success(lease, 1).await?;
    f.failure(lease, 2).await?;
    let sealed = jobs::seal_results(&f.bed.pg, lease).await?;
    assert_eq!((sealed.success_count, sealed.failure_count), (2, 1));
    assert!(jobs::finish(&f.bed.pg, lease).await.is_err());
    assert!(f.success(lease, 1).await.is_err());
    f.settle(&sealed, 2).await?;
    let completed = jobs::finish(&f.bed.pg, lease).await?;
    assert_eq!(completed.state, State::Partial);
    assert_eq!(completed.actual_micro, Some(1000));
    assert_eq!(f.bed.wallet().await?, 9000);
    f.bed.evidence(row.id, 1000).await?;
    let (_, mime) = jobs::content_owned(&f.bed.pg, row.id, f.bed.uid, f.bed.kid, 0)
        .await?
        .ok_or("missing image")?;
    assert_eq!(mime, "image/png");
    assert!(
        jobs::content_owned(&f.bed.pg, row.id, f.bed.uid, f.bed.kid + 1, 0)
            .await?
            .is_none()
    );
    assert!(
        jobs::content_owned(&f.bed.pg, row.id, f.bed.uid + 1, f.bed.kid, 0)
            .await?
            .is_none()
    );
    assert!(jobs::claim(&f.bed.pg, Some(row.id)).await?.is_none());
    sqlx::query("UPDATE image_batches SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(row.id)
        .execute(&f.bed.pg)
        .await?;
    assert!(
        jobs::content_owned(&f.bed.pg, row.id, f.bed.uid, f.bed.kid, 0)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn native_unknown_conflicting_outputs_and_contradictory_terminals_fail_closed() -> TestResult
{
    let f = Fixture::new().await?;
    let (row, lease) = f.collecting(RemoteState::Succeeded).await?;
    assert!(f.success(lease, 3).await.is_err());
    f.success(lease, 0).await?;
    assert!(f.failure(lease, 0).await.is_err());
    assert!(
        jobs::stage(
            &f.bed.pg,
            lease,
            0,
            Output::Success {
                content: b"different",
                content_type: "image/png",
                usage: &json!({})
            }
        )
        .await
        .is_err()
    );
    assert!(
        jobs::observe(
            &f.bed.pg,
            row.id,
            row.submit_intent.ok_or("missing intent")?,
            Observation {
                job_name: "batches/test",
                state: RemoteState::Cancelled,
                output_ref: &json!({})
            }
        )
        .await
        .is_err()
    );
    let old = jobs::observe(
        &f.bed.pg,
        row.id,
        row.submit_intent.ok_or("missing intent")?,
        Observation {
            job_name: "batches/test",
            state: RemoteState::Running,
            output_ref: &json!({}),
        },
    )
    .await?;
    assert_eq!(old.state, State::Collecting);
    assert_eq!(old.remote_state.as_deref(), Some("succeeded"));
    assert!(
        jobs::content_owned(&f.bed.pg, row.id, f.bed.uid, f.bed.kid, 0)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn native_cancel_before_submit_closes_hold_before_publishing_zero_charge() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = f.preparing().await?;
    jobs::cancel(&f.bed.pg, row.id, f.bed.uid, f.bed.kid)
        .await?
        .ok_or("missing cancel")?;
    assert!(jobs::mark_submitting(&f.bed.pg, lease).await.is_err());
    assert_eq!(f.bed.wallet().await?, 8500);
    let sealed = jobs::abort_before_submission(&f.bed.pg, lease, "cancelled").await?;
    assert_eq!(sealed.state, State::Settling);
    assert!(jobs::finish(&f.bed.pg, lease).await.is_err());
    f.settle(&sealed, 0).await?;
    let done = jobs::finish(&f.bed.pg, lease).await?;
    assert_eq!(done.state, State::Cancelled);
    assert_eq!(done.actual_micro, Some(0));
    assert_eq!(f.bed.wallet().await?, 10_000);
    assert!(done.provider_job_name.is_none());
    Ok(())
}

#[tokio::test]
async fn native_best_effort_cancel_cannot_refund_a_successful_remote_job() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = f.collecting(RemoteState::Succeeded).await?;
    jobs::cancel(&f.bed.pg, row.id, f.bed.uid, f.bed.kid).await?;
    assert!(
        jobs::abort_before_submission(&f.bed.pg, lease, "cancelled")
            .await
            .is_err()
    );
    for slot in 0..3 {
        f.success(lease, slot).await?;
    }
    let sealed = jobs::seal_results(&f.bed.pg, lease).await?;
    f.settle(&sealed, 3).await?;
    let done = jobs::finish(&f.bed.pg, lease).await?;
    assert_eq!(done.state, State::Completed);
    assert_eq!(done.actual_micro, Some(1500));
    assert_eq!(f.bed.wallet().await?, 8500);
    Ok(())
}

#[tokio::test]
async fn native_closed_hold_with_incorrect_unit_count_cannot_publish_results() -> TestResult {
    let f = Fixture::new().await?;
    let (row, lease) = f.collecting(RemoteState::Succeeded).await?;
    for slot in 0..3 {
        f.success(lease, slot).await?;
    }
    let sealed = jobs::seal_results(&f.bed.pg, lease).await?;
    f.settle(&sealed, 2).await?;
    assert!(matches!(
        jobs::finish(&f.bed.pg, lease).await,
        Err(jobs::Error::NotSettled)
    ));
    assert!(
        jobs::content_owned(&f.bed.pg, row.id, f.bed.uid, f.bed.kid, 0)
            .await?
            .is_none()
    );
    Ok(())
}

#[test]
fn native_unit_quote_preserves_surcharges_and_rejects_overflow() {
    let quote = UnitQuote {
        amount: 600,
        original: 500,
        discount: -100,
        list_price: 500,
        upstream_cost: Some(300),
    };
    assert_eq!(quote.total(3).expect("valid surcharge").discount, -300);
    assert_eq!(quote.total(0).expect("zero output").amount, 0);
    assert!(quote.total(201).is_err());
    assert!(
        UnitQuote {
            amount: 9_007_199_254_740_991,
            original: 9_007_199_254_740_991,
            discount: 0,
            ..quote.clone()
        }
        .total(2)
        .is_err()
    );
    assert!(
        UnitQuote {
            discount: i64::MIN,
            ..quote
        }
        .total(1)
        .is_err()
    );
}
