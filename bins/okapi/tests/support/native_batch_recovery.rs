use super::*;

async fn lost_ack(env: &Env, count: u32) -> Value {
    env.peer.lock().unwrap().create_status = 502;
    let job = env.submit(count, "lost-ack").await;
    env.step(&job).await.unwrap();
    assert!(env.step(&job).await.is_err());
    assert_eq!(env.poll(&job).await["status"], "uncertain");
    assert_eq!(env.creates(), 1);
    job
}
fn candidate(env: &Env) -> Value {
    let peer = env.peer.lock().unwrap();
    let (name, remote) = peer.jobs.iter().next().unwrap();
    json!({"name":name,"metadata":remote.metadata()})
}
async fn still_held(env: &Env, job: &Value, outputs: i64) {
    assert_eq!(env.poll(job).await["status"], "uncertain");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE request_id=$1")
        .bind(id(job))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        BALANCE - PRICE * outputs / 2
    );
    assert_eq!(env.creates(), 1);
    assert_eq!(
        env.request(reqwest::Method::GET, &path(job, "/content/0"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}
async fn finish(env: &Env, job: &Value, amount: i64) {
    assert_eq!(env.poll(job).await["status"], "collecting");
    env.step(job).await.unwrap();
    env.step(job).await.unwrap();
    env.money(job, amount).await;
    assert!(!env.step(job).await.unwrap());
    assert_eq!(env.creates(), 1);
}

#[tokio::test]
async fn lost_ack_recovers_only_after_all_pages_and_survives_worker_restart() {
    let env = Env::new().await;
    env.peer.lock().unwrap().mode = "partial".into();
    let job = lost_ack(&env, 3).await;
    let row = candidate(&env);
    env.peer.lock().unwrap().list_pages = Some(vec![
        (200, json!({"operations":[row],"nextPageToken":"page-1"})),
        (200, json!({"operations":[]})),
    ]);
    env.step(&job).await.unwrap();
    still_held(&env, &job, 3).await;
    let scan: (i32, bool) =
        sqlx::query_as("SELECT pages,complete FROM image_batch_recovery WHERE batch_id=$1")
            .bind(id(&job))
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(scan, (1, false));
    // Reconfiguration must not change the frozen upstream account or quote.
    sqlx::query("UPDATE channels SET api_base='http://127.0.0.1:1/v1beta'")
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE model_pricing SET per_call_price_micro=999999")
        .execute(&env.state.pg)
        .await
        .unwrap();
    let restarted = gateway::build_state(
        &env.database,
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "recovery-host",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(run_one(&restarted, Some(id(&job))).await.unwrap());
    restarted.pg.close().await;
    assert_eq!(
        env.peer.lock().unwrap().list_queries,
        [None, Some("page-1".into())]
    );
    finish(&env, &job, PRICE / 2).await;
    assert_eq!(env.poll(&job).await["status"], "partial");
    let public = env.poll(&job).await.to_string();
    for private in [
        "submit-label",
        "submit_intent",
        "page-1",
        "candidate_name",
        "batch-private-credential",
    ] {
        assert!(!public.contains(private));
    }
    env.close().await;
}

#[tokio::test]
async fn distinct_candidates_across_pages_are_sticky_and_never_adopted() {
    let env = Env::new().await;
    let job = lost_ack(&env, 1).await;
    let row = candidate(&env);
    let mut second = row.clone();
    second["name"] = json!("batches/another-job");
    env.peer.lock().unwrap().list_pages = Some(vec![
        (200, json!({"operations":[row],"nextPageToken":"page-1"})),
        (200, json!({"operations":[second]})),
    ]);
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    still_held(&env, &job, 1).await;
    env.peer.lock().unwrap().list_pages = None;
    env.step(&job).await.unwrap();
    still_held(&env, &job, 1).await;
    assert_eq!(env.peer.lock().unwrap().list_queries.len(), 2);
    let conflict: bool =
        sqlx::query_scalar("SELECT conflict FROM image_batch_recovery WHERE batch_id=$1")
            .bind(id(&job))
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert!(conflict);
    env.close().await;
}

#[tokio::test]
async fn matching_list_cannot_override_mismatched_fresh_job_identity() {
    let env = Env::new().await;
    let job = lost_ack(&env, 1).await;
    let mut metadata = candidate(&env)["metadata"].clone();
    metadata["inputConfig"]["fileName"] = json!("files/another-job");
    env.peer.lock().unwrap().get_metadata = Some(metadata);
    env.step(&job).await.unwrap();
    still_held(&env, &job, 1).await;
    env.peer.lock().unwrap().get_metadata = None;
    env.step(&job).await.unwrap();
    still_held(&env, &job, 1).await;
    assert_eq!(env.peer.lock().unwrap().list_queries.len(), 1);
    env.close().await;
}

#[tokio::test]
async fn missing_list_and_temporary_get_404_wait_without_refund_then_recover() {
    let env = Env::new().await;
    let job = lost_ack(&env, 1).await;
    env.peer.lock().unwrap().list_pages = Some(vec![(200, json!({}))]);
    env.step(&job).await.unwrap();
    still_held(&env, &job, 1).await;
    {
        let mut peer = env.peer.lock().unwrap();
        peer.list_pages = None;
        peer.get_status = Some(404);
    }
    env.step(&job).await.unwrap();
    still_held(&env, &job, 1).await;
    env.peer.lock().unwrap().get_status = None;
    env.step(&job).await.unwrap();
    finish(&env, &job, PRICE / 2).await;
    assert_eq!(env.peer.lock().unwrap().list_queries.len(), 2);
    env.close().await;
}

#[tokio::test]
async fn malformed_page_cannot_complete_scan_and_expired_cursor_can_restart_it() {
    let env = Env::new().await;
    let job = lost_ack(&env, 1).await;
    let row = candidate(&env);
    env.peer.lock().unwrap().list_pages = Some(vec![
        (200, json!({"operations":[row],"nextPageToken":"page-1"})),
        (200, json!([])),
    ]);
    env.step(&job).await.unwrap();
    assert!(env.step(&job).await.is_err());
    still_held(&env, &job, 1).await;
    env.peer.lock().unwrap().list_pages.as_mut().unwrap()[1] = (400, json!({"error":"expired"}));
    env.step(&job).await.unwrap();
    still_held(&env, &job, 1).await;
    env.peer.lock().unwrap().list_pages = None;
    env.step(&job).await.unwrap();
    finish(&env, &job, PRICE / 2).await;
    assert_eq!(
        env.peer.lock().unwrap().list_queries,
        [None, Some("page-1".into()), Some("page-1".into()), None]
    );
    env.close().await;
}

#[tokio::test]
async fn recovery_honors_pending_cancel_even_after_api_key_revocation() {
    let env = Env::new().await;
    env.peer.lock().unwrap().mode = "running".into();
    let job = lost_ack(&env, 1).await;
    env.value(reqwest::Method::POST, &path(&job, "/cancel"))
        .await;
    sqlx::query("UPDATE api_keys SET status=2 WHERE id=$1")
        .bind(env.kid)
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.step(&job).await.unwrap(); // find original running job
    env.step(&job).await.unwrap(); // cancel and collect terminal result
    env.step(&job).await.unwrap(); // close hold
    env.money(&job, 0).await;
    let status: String = sqlx::query_scalar("SELECT state FROM image_batches WHERE id=$1")
        .bind(id(&job))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    assert_eq!(env.creates(), 1);
    assert_eq!(
        env.peer
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(_, p, _)| p.ends_with(":cancel"))
            .count(),
        1
    );
    env.close().await;
}

#[tokio::test]
async fn cancelled_batch_metadata_outputs_bill_only_delivered_successes() {
    let env = Env::new().await;
    env.peer.lock().unwrap().mode = "cancelled_partial".into();
    let job = lost_ack(&env, 3).await;
    env.step(&job).await.unwrap();
    finish(&env, &job, PRICE / 2).await;
    assert_eq!(env.poll(&job).await["status"], "partial");
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&job, "/content/0"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    env.close().await;
}
