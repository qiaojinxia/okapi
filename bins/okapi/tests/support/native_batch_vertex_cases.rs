use super::*;

#[tokio::test]
async fn vertex_partial_results_accept_failure_status_with_empty_response() {
    let v = VertexEnv::new().await;
    let job = v.start(3).await;
    v.peer.lock().unwrap().results(1);
    v.env
        .step(&job)
        .await
        .expect("official failed rows must not invalidate a whole batch");
    v.env.step(&job).await.unwrap();
    v.env.money(&job, PRICE / 2).await;
    assert_eq!(v.env.poll(&job).await["status"], "partial");
    let items = v
        .env
        .value(reqwest::Method::GET, &path(&job, "/items"))
        .await;
    assert!(!items.to_string().contains("private-provider-detail"));
    assert_eq!(v.peer.lock().unwrap().creates, 1);
    assert!(v.peer.lock().unwrap().tokens >= 2);
    v.env.close().await;
}

#[tokio::test]
async fn vertex_collection_uses_reported_directory_and_exact_object_generation() {
    let v = VertexEnv::new().await;
    let job = v.start(2).await;
    {
        let mut peer = v.peer.lock().unwrap();
        peer.results(2);
        let root = peer.job["outputConfig"]["gcsDestination"]["outputUriPrefix"]
            .as_str()
            .unwrap()
            .strip_prefix(&format!("gs://{BUCKET}/"))
            .unwrap()
            .to_owned();
        peer.put(
            format!("{root}sibling/predictions.jsonl"),
            "not from this output directory",
        );
    }
    v.env
        .step(&job)
        .await
        .expect("only the reported directory belongs to these results");
    v.env.step(&job).await.unwrap();
    v.env.money(&job, PRICE).await;
    let download = v
        .env
        .request(reqwest::Method::GET, &path(&job, "/content/0"))
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), 200);
    assert_eq!(download.headers()["cache-control"], "private, no-store");
    assert_eq!(download.bytes().await.unwrap().as_ref(), PNG);
    {
        let peer = v.peer.lock().unwrap();
        assert_eq!(peer.list_prefixes, [peer.output_dir(), peer.output_dir()]);
        assert_eq!(peer.creates, 1);
    }
    v.env.close().await;
}

#[tokio::test]
async fn vertex_aggregate_result_budget_cannot_be_reset_per_file() {
    let v = VertexEnv::new().await;
    let job = v.start(1).await;
    let stored: i64 = sqlx::query_scalar("SELECT storage_budget FROM image_batches WHERE id=$1")
        .bind(id(&job))
        .fetch_one(&v.env.state.pg)
        .await
        .unwrap();
    let budget = usize::try_from(stored).unwrap() * 2;
    {
        let mut peer = v.peer.lock().unwrap();
        let row = format!("{}\n", VertexPeer::row(&peer.keys()[0], true));
        let mut first = vec![b' '; budget / 2];
        first.extend_from_slice(row.as_bytes());
        let dir = peer.output_dir();
        peer.put(format!("{dir}predictions_0001.jsonl"), first);
        peer.put(
            format!("{dir}predictions_0002.jsonl"),
            vec![b' '; budget / 2],
        );
    }
    assert!(
        v.env.step(&job).await.is_err(),
        "each file fits, but the combined response exceeds the job budget"
    );
    v.private(&job).await;
    v.env.close().await;
}

#[tokio::test]
async fn vertex_success_with_missing_result_files_stays_private_for_retry() {
    let v = VertexEnv::new().await;
    let job = v.start(2).await;
    assert!(
        v.env.step(&job).await.is_err(),
        "success without all results is not evidence for a refund"
    );
    v.private(&job).await;
    v.peer.lock().unwrap().results(2);
    v.env.step(&job).await.unwrap();
    v.env.step(&job).await.unwrap();
    v.env.money(&job, PRICE).await;
    v.env.close().await;
}

#[tokio::test]
async fn vertex_lost_submit_ack_resumes_paginated_lookup_after_restart() {
    let v = VertexEnv::new().await;
    v.peer.lock().unwrap().create_status = 502;
    let job = v.env.submit(1, "lost-vertex").await;
    v.env.step(&job).await.unwrap();
    assert!(v.env.step(&job).await.is_err());
    {
        let mut peer = v.peer.lock().unwrap();
        peer.results(1);
        peer.list_pages = Some(vec![
            json!({"batchPredictionJobs":[peer.job],"nextPageToken":"1"}),
            json!({}),
        ]);
    }
    v.env.step(&job).await.unwrap();
    assert_eq!(v.env.poll(&job).await["status"], "uncertain");
    sqlx::query("UPDATE model_pricing SET per_call_price_micro=999999")
        .execute(&v.env.state.pg)
        .await
        .unwrap();
    let restarted = gateway::build_state(
        &v.env.database,
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "vertex-recovery",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(run_one(&restarted, Some(id(&job))).await.unwrap());
    restarted.pg.close().await;
    assert_eq!(v.env.poll(&job).await["status"], "collecting");
    v.env.step(&job).await.unwrap();
    v.env.step(&job).await.unwrap();
    v.env.money(&job, PRICE / 2).await;
    assert_eq!(v.peer.lock().unwrap().creates, 1);
    v.env.close().await;
}

#[tokio::test]
async fn vertex_cancel_preserves_completed_rows_and_refunds_only_unused_outputs() {
    for count in [0, 1] {
        let v = VertexEnv::new().await;
        let job = v.start(3).await;
        {
            let mut peer = v.peer.lock().unwrap();
            peer.state = "JOB_STATE_RUNNING".into();
            if count > 0 {
                let row = VertexPeer::row(&peer.keys()[0], true);
                let dir = peer.output_dir();
                peer.put(format!("{dir}predictions.jsonl"), format!("{row}\n"));
            }
        }
        v.env
            .value(reqwest::Method::POST, &path(&job, "/cancel"))
            .await;
        v.env.step(&job).await.unwrap();
        v.env.step(&job).await.unwrap();
        v.env.money(&job, PRICE * count / 2).await;
        assert_eq!(
            v.env.poll(&job).await["status"],
            if count == 0 { "cancelled" } else { "partial" }
        );
        assert_eq!(v.peer.lock().unwrap().creates, 1);
        v.env.close().await;
    }
}

#[tokio::test]
async fn vertex_late_page_errors_and_truncated_bodies_never_publish_staged_images() {
    for truncate in [false, true] {
        let v = VertexEnv::new().await;
        let job = v.start(2).await;
        {
            let mut peer = v.peer.lock().unwrap();
            peer.results(2);
            peer.malformed_page = !truncate;
            peer.truncated = truncate;
        }
        assert!(v.env.step(&job).await.is_err());
        v.private(&job).await;
        {
            let mut peer = v.peer.lock().unwrap();
            peer.malformed_page = false;
            peer.truncated = false;
        }
        v.env.step(&job).await.unwrap();
        v.env.step(&job).await.unwrap();
        v.env.money(&job, PRICE).await;
        assert!(!v.env.step(&job).await.unwrap());
        v.env.close().await;
    }
}

#[tokio::test]
async fn vertex_duplicate_rows_and_error_with_real_response_are_rejected() {
    for mode in ["duplicate", "conflict", "shape"] {
        let duplicate = mode == "duplicate";
        let v = VertexEnv::new().await;
        let job = v.start(2).await;
        {
            let mut peer = v.peer.lock().unwrap();
            let keys = peer.keys();
            let dir = peer.output_dir();
            let first = VertexPeer::row(&keys[0], true);
            peer.put(format!("{dir}predictions_0001.jsonl"), format!("{first}\n"));
            let mut second = VertexPeer::row(&keys[usize::from(!duplicate)], true);
            if mode == "conflict" {
                second["status"] = json!("failed");
            } else if mode == "shape" {
                second["response"] = json!([]);
            }
            peer.put(
                format!("{dir}predictions_0002.jsonl"),
                format!("{second}\n"),
            );
        }
        assert!(v.env.step(&job).await.is_err());
        v.private(&job).await;
        v.env.close().await;
    }
}

#[tokio::test]
async fn vertex_input_upload_ack_loss_verifies_same_generation_before_only_submission() {
    let v = VertexEnv::new().await;
    v.peer.lock().unwrap().upload_status_once = Some(502);
    let job = v.env.submit(1, "upload-loss").await;
    v.env.step(&job).await.unwrap();
    assert!(v.env.step(&job).await.is_err());
    assert_eq!(v.env.poll(&job).await["status"], "preparing");
    assert_eq!(v.peer.lock().unwrap().creates, 0);
    v.env.step(&job).await.unwrap();
    {
        let mut peer = v.peer.lock().unwrap();
        assert_eq!(peer.creates, 1);
        assert_eq!(peer.objects.len(), 1);
        let reads = peer
            .calls
            .iter()
            .filter(|(method, _, query)| {
                *method == Method::GET && query.get("alt").map(String::as_str) == Some("media")
            })
            .count();
        assert_eq!(reads, 1, "reused input must be read and hashed");
        peer.results(1);
    }
    v.env.step(&job).await.unwrap();
    v.env.step(&job).await.unwrap();
    v.env.money(&job, PRICE / 2).await;
    v.env.close().await;
}
