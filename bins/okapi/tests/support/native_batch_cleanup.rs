use super::*;

impl Env {
    pub(super) async fn cleanup(&self, job: &Value) -> Result<bool, gateway::error::AppError> {
        sqlx::query("UPDATE image_batches SET next_run_at=now() WHERE id=$1")
            .bind(id(job))
            .execute(&self.state.pg)
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(15),
            gateway::images::batches::run_cleanup(&self.state, Some(id(job))),
        )
        .await
        .unwrap()
    }
    pub(super) async fn cleaned(&self, job: &Value) -> bool {
        sqlx::query_scalar("SELECT cleanup_done FROM image_batches WHERE id=$1")
            .bind(id(job))
            .fetch_one(&self.state.pg)
            .await
            .unwrap()
    }
    pub(super) async fn artifacts(&self, job: &Value) -> (i64, i64) {
        sqlx::query_as("SELECT (SELECT COUNT(*) FROM image_batch_payloads WHERE batch_id=$1),(SELECT COUNT(*) FROM image_batch_outputs WHERE batch_id=$1)").bind(id(job)).fetch_one(&self.state.pg).await.unwrap()
    }
}
async fn completed(env: &Env, idem: &str) -> Value {
    let job = env.submit(1, idem).await;
    for _ in 0..4 {
        env.step(&job).await.unwrap();
    }
    assert_eq!(env.poll(&job).await["status"], "completed");
    job
}

#[tokio::test]
async fn gemini_delete_recovers_lost_ack_and_file_failure_without_rebilling() {
    let env = Env::new().await;
    let job = completed(&env, "cleanup-ack").await;
    let pricing = env.money(&job, PRICE / 2).await;
    {
        let mut peer = env.peer.lock().unwrap();
        peer.files.insert(
            "files/foreign".into(),
            Bytes::from_static(b"other user's file"),
        );
        peer.delete_status_once = Some(503);
        peer.delete_file_status_once = Some(403);
    }
    assert!(!env.cleanup(&job).await.unwrap());
    let deleted = env.value(reqwest::Method::DELETE, &path(&job, "")).await;
    assert_eq!(deleted["cleanup_pending"], true);
    assert!(env.cleanup(&job).await.is_err()); // Remote job deleted, acknowledgement lost.
    assert_eq!(env.artifacts(&job).await, (1, 1));
    assert!(env.cleanup(&job).await.is_err()); // GET 404; file permission failure retains binding.
    assert!(!env.cleaned(&job).await);
    assert_eq!(env.money(&job, PRICE / 2).await, pricing);
    assert!(env.cleanup(&job).await.unwrap());
    assert!(env.cleaned(&job).await);
    assert_eq!(env.artifacts(&job).await, (0, 0));
    assert_eq!(env.money(&job, PRICE / 2).await, pricing);
    assert!(!env.cleanup(&job).await.unwrap());
    assert_eq!(
        env.value(reqwest::Method::DELETE, &path(&job, "")).await["cleanup_pending"],
        false
    );
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&job, "/content/0"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let replay = env
        .request(reqwest::Method::POST, "/v1/images/batches")
        .header("idempotency-key", "cleanup-ack")
        .json(&env.body(1))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 409);
    assert_eq!(env.creates(), 1);
    assert_eq!(
        env.peer
            .lock()
            .unwrap()
            .files
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["files/foreign"]
    );
    env.close().await;
}

#[tokio::test]
async fn gemini_expiry_purges_artifacts_preserves_history_and_idempotent_replay() {
    let env = Env::new().await;
    env.peer.lock().unwrap().mode = "file".into();
    let job = completed(&env, "expiry").await;
    assert_eq!(env.peer.lock().unwrap().files.len(), 2);
    sqlx::query("UPDATE image_batches SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(id(&job))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(
        env.request(reqwest::Method::GET, &path(&job, "/content/0"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let (stop, rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(gateway::images::batches::run_worker(env.state.clone(), rx));
    tokio::time::timeout(Duration::from_secs(8), async {
        while !env.cleaned(&job).await {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("worker must perform expiry cleanup automatically");
    stop.send(true).unwrap();
    worker.await.unwrap();
    assert!(env.cleaned(&job).await);
    assert_eq!(env.poll(&job).await["status"], "completed");
    assert_eq!(env.poll(&job).await["cleanup_done"], true);
    assert!(env.peer.lock().unwrap().files.is_empty());
    assert_eq!(env.submit(1, "expiry").await["id"], job["id"]);
    assert_eq!(env.artifacts(&job).await, (0, 0));
    assert_eq!(env.creates(), 1);
    env.money(&job, PRICE / 2).await;
    env.close().await;
}

#[tokio::test]
async fn cancelled_before_submit_cleanup_does_not_create_remote_work_or_charge() {
    let env = Env::new().await;
    let job = env.submit(1, "cancel-cleanup").await;
    env.value(reqwest::Method::POST, &path(&job, "/cancel"))
        .await;
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "cancelled");
    env.value(reqwest::Method::DELETE, &path(&job, "")).await;
    assert!(env.cleanup(&job).await.unwrap());
    assert!(env.cleaned(&job).await);
    assert_eq!(env.artifacts(&job).await, (0, 0));
    assert_eq!(env.creates(), 0);
    env.money(&job, 0).await;
    env.close().await;
}
