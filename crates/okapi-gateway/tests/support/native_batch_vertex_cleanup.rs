use super::*;

async fn completed(v: &VertexEnv) -> Value {
    let job = v.start(2).await;
    v.peer.lock().unwrap().results(2);
    v.env.step(&job).await.unwrap();
    v.env.step(&job).await.unwrap();
    assert_eq!(v.env.poll(&job).await["status"], "completed");
    v.env.money(&job, PRICE).await;
    v.env.value(reqwest::Method::DELETE, &path(&job, "")).await;
    job
}
async fn drain(v: &VertexEnv, job: &Value) {
    for _ in 0..20 {
        if v.env.cleaned(job).await {
            return;
        }
        assert!(v.env.cleanup(job).await.unwrap());
    }
    panic!("cleanup did not finish");
}

#[tokio::test]
async fn vertex_cleanup_waits_for_deletion_then_removes_all_versions_without_skipping() {
    let v = VertexEnv::new().await;
    let job = completed(&v).await;
    let foreign = "okapi-batches/foreign/output/keep.jsonl";
    {
        let mut peer = v.peer.lock().unwrap();
        let keys: Vec<_> = peer.objects.keys().cloned().collect();
        for key in keys {
            peer.old_versions.insert(
                (key, "100".into()),
                Object {
                    bytes: Bytes::from_static(b"old private input/output"),
                    generation: "100".into(),
                },
            );
        }
        peer.put(foreign.into(), "foreign task");
        let root = format!(
            "okapi-batches/{}/output/extra/result.jsonl",
            id(&job).simple()
        );
        peer.put(root, "output outside the concrete result directory");
    }
    let before = v.peer.lock().unwrap().objects.len();
    assert!(v.env.cleanup(&job).await.unwrap());
    assert!(v.peer.lock().unwrap().deletion.requested);
    assert_eq!(v.peer.lock().unwrap().objects.len(), before);
    // A new worker state must keep the persisted LRO and original account even
    // after an administrator changes the channel's connection settings.
    sqlx::query("UPDATE channels SET api_base='http://127.0.0.1:1/v1beta'")
        .execute(&v.env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE image_batches SET next_run_at=now() WHERE id=$1")
        .bind(id(&job))
        .execute(&v.env.state.pg)
        .await
        .unwrap();
    let restarted = gateway::build_state(
        &v.env.database,
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "cleanup-restart",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        gateway::images::batches::run_cleanup(&restarted, Some(id(&job)))
            .await
            .unwrap()
    );
    restarted.pg.close().await;
    assert_eq!(v.peer.lock().unwrap().objects.len(), before);
    assert_eq!(v.env.artifacts(&job).await, (1, 2));
    v.peer.lock().unwrap().deletion.state = DeletionState::Complete;
    assert!(v.env.cleanup(&job).await.unwrap()); // LRO done; fresh job absence still required.
    assert_eq!(v.peer.lock().unwrap().objects.len(), before);
    drain(&v, &job).await;
    assert_eq!(v.env.artifacts(&job).await, (0, 0));
    assert!(!v.env.cleanup(&job).await.unwrap());
    {
        let peer = v.peer.lock().unwrap();
        assert!(peer.old_versions.is_empty());
        assert_eq!(
            peer.objects.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![foreign]
        );
        assert_eq!(peer.creates, 1);
        assert_eq!(
            peer.calls
                .iter()
                .filter(|(m, p, _)| *m == Method::DELETE && p.starts_with("/v1/"))
                .count(),
            1
        );
        for (_, _, query) in peer
            .calls
            .iter()
            .filter(|(m, p, _)| *m == Method::DELETE && p.starts_with("/storage/"))
        {
            assert!(query.contains_key("generation"));
        }
    }
    v.env.money(&job, PRICE).await;
    v.env.close().await;
}

#[tokio::test]
async fn vertex_cleanup_rejects_changed_remote_state_and_incomplete_namespace_scans() {
    let v = VertexEnv::new().await;
    let job = completed(&v).await;
    let before = v.peer.lock().unwrap().objects.len();
    for state in ["JOB_STATE_RUNNING", "JOB_STATE_FAILED"] {
        v.peer.lock().unwrap().state = state.into();
        assert!(v.env.cleanup(&job).await.is_err());
        assert!(!v.peer.lock().unwrap().deletion.requested);
    }
    {
        let mut peer = v.peer.lock().unwrap();
        peer.state = "JOB_STATE_SUCCEEDED".into();
        peer.deletion.gone = true;
    }
    for pages in [
        vec![
            json!({"items":[],"nextPageToken":"1"}),
            json!({"items":[],"nextPageToken":"2"}),
            json!({"items":[],"nextPageToken":"1"}),
        ],
        vec![
            json!({"items":[{"name":"okapi-batches/another/output/secret","bucket":BUCKET,"generation":"7"}]}),
        ],
        vec![
            json!({"items":[],"nextPageToken":"1"}),
            json!({"items":"malformed"}),
        ],
    ] {
        v.peer.lock().unwrap().cleanup_pages = Some(pages);
        assert!(v.env.cleanup(&job).await.is_err());
        assert_eq!(v.env.artifacts(&job).await, (1, 2));
        assert!(!v.env.cleaned(&job).await);
        assert_eq!(v.peer.lock().unwrap().objects.len(), before);
    }
    v.peer.lock().unwrap().cleanup_pages = None;
    drain(&v, &job).await;
    v.env.money(&job, PRICE).await;
    v.env.close().await;
}

#[tokio::test]
async fn vertex_cleanup_retries_failed_missing_operations_and_lost_delete_responses() {
    let v = VertexEnv::new().await;
    let job = completed(&v).await;
    let before = v.peer.lock().unwrap().objects.len();
    v.env.cleanup(&job).await.unwrap();
    v.peer.lock().unwrap().deletion.state = DeletionState::Missing;
    v.env.cleanup(&job).await.unwrap();
    assert_eq!(v.peer.lock().unwrap().objects.len(), before);
    v.peer.lock().unwrap().deletion.state = DeletionState::Pending;
    v.env.cleanup(&job).await.unwrap(); // Reissue exact job delete, not create.
    v.peer.lock().unwrap().deletion.state = DeletionState::Failed;
    assert!(v.env.cleanup(&job).await.is_err());
    assert_eq!(v.peer.lock().unwrap().objects.len(), before);
    {
        let mut peer = v.peer.lock().unwrap();
        peer.deletion.state = DeletionState::Pending;
        peer.deletion.status_once = Some(503); // Job gone, acknowledgement lost.
        peer.object_delete_status_once = Some(503); // Object gone, acknowledgement lost.
    }
    assert!(v.env.cleanup(&job).await.is_err());
    assert!(v.env.cleanup(&job).await.is_err());
    assert_eq!(v.env.artifacts(&job).await, (1, 2));
    assert!(!v.env.cleaned(&job).await);
    drain(&v, &job).await;
    assert!(v.peer.lock().unwrap().objects.is_empty());
    assert_eq!(v.peer.lock().unwrap().creates, 1);
    v.env.money(&job, PRICE).await;
    v.env.close().await;
}
