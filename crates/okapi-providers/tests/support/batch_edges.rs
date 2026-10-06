use super::*;
use okapi_providers::batch::{BatchError, Job, MAX_INLINE_BYTES, MAX_INPUT_BYTES};

async fn gemini_job(
    server: &mut Server,
    client: &GeminiBatch,
    value: Value,
) -> Result<Job, BatchError> {
    let c = client.clone();
    let call = tokio::spawn(async move { c.get("batches/job").await });
    server.next().await.json(value);
    call.await.unwrap()
}

#[test]
fn configuration_refuses_ambiguous_authorities_versions_and_namespaces() {
    let out = Outbound::default();
    for base in [
        "https://user:secret@host/v1beta",
        "https://host/v1beta?key=x",
        "https://host/v1beta#x",
        "file:///v1beta",
        "https://host/v1",
    ] {
        assert!(GeminiBatch::new(base, "key", &out).is_err());
    }
    assert!(GeminiBatch::new("https://host/v1beta", "", &out).is_err());
    assert!(
        VertexBatch::new(
            "https://host/v1/projects/p/locations/l/extra",
            "token",
            &out
        )
        .is_err()
    );
    assert!(GcsStore::new("https://storage.test", "token", &out, "bucket", "../escape").is_err());
    assert!(
        GcsStore::new(
            "https://storage.test",
            "token",
            &out,
            "bucket?override=x",
            ID
        )
        .is_err()
    );
    let out = Outbound {
        proxy_url: Some("file:///proxy".into()),
        extra_headers: vec![],
        ..Default::default()
    };
    assert!(GeminiBatch::new("https://host/v1beta", "key", &out).is_err());
}

#[tokio::test]
async fn oversized_inputs_fail_while_serializing_without_sending_a_request() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let mut rows = requests();
    rows[0].request["contents"][0]["parts"][0]["text"] =
        Value::String("x".repeat(MAX_INLINE_BYTES));
    let error = client
        .create_inline("image", "pictures", &rows)
        .await
        .unwrap_err();
    assert_eq!(error.code, "batch_inline_size");
    assert!(!error.may_have_executed);
    rows[0].request["contents"][0]["parts"][0]["text"] = Value::String("x".repeat(MAX_INPUT_BYTES));
    assert_eq!(jsonl::encode(&rows).unwrap_err().code, "batch_input_size");
    drop(rows);
    server.quiet().await;
}

#[tokio::test]
async fn upload_headers_length_and_returned_file_identity_remain_controlled() {
    let mut server = Server::new().await;
    let outbound = Outbound {
        proxy_url: None,
        extra_headers: vec![
            ("x-goog-api-key".into(), "evil".into()),
            ("authorization".into(), "Bearer evil".into()),
            ("x-goog-upload-command".into(), "evil".into()),
            ("X-Goog-Upload-Offset".into(), "999".into()),
            ("X-Custom".into(), "kept".into()),
        ],
        ..Default::default()
    };
    let client = GeminiBatch::new(
        &format!("{}/proxy/v1beta", server.base),
        "real-key",
        &outbound,
    )
    .unwrap();
    let c = client.clone();
    let call = tokio::spawn(async move { c.start_upload("files/input", "pictures", 3).await });
    let req = server.next().await;
    assert_eq!(req.headers["x-goog-api-key"], "real-key");
    assert!(!req.headers.contains_key("authorization"));
    assert_eq!(req.headers["x-goog-upload-command"], "start");
    assert_eq!(req.headers["x-custom"], "kept");
    assert!(!req.headers.contains_key("x-goog-upload-offset"));
    let mut reply = Reply::empty(200);
    reply.headers.push((
        "x-goog-upload-url",
        format!(
            "{}/proxy/upload/v1beta/files?upload_id=private",
            server.base
        ),
    ));
    req.respond(reply);
    let session = call.await.unwrap().unwrap();
    assert_eq!(
        client
            .finish_upload(&session, Bytes::from_static(b"wrong-length"))
            .await
            .unwrap_err()
            .code,
        "batch_upload_length"
    );
    let c = client.clone();
    let call =
        tokio::spawn(async move { c.finish_upload(&session, Bytes::from_static(b"{}\n")).await });
    let req = server.next().await;
    assert_eq!(req.headers["x-goog-upload-command"], "upload, finalize");
    assert_eq!(req.headers["x-goog-upload-offset"], "0");
    req.json(json!({"file":{"name":"files/other","state":"ACTIVE"}}));
    let error = call.await.unwrap().unwrap_err();
    assert_eq!(error.code, "batch_file_identity");
    assert!(error.may_have_executed);
    server.quiet().await;
}

#[tokio::test]
async fn state_variants_keep_failed_expired_and_partial_results_distinct() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    for (state, code, expected) in [
        ("BATCH_STATE_FAILED", 13, JobState::Failed),
        ("BATCH_STATE_CANCELLED", 1, JobState::Cancelled),
        ("BATCH_STATE_EXPIRED", 4, JobState::Expired),
    ] {
        let job=gemini_job(&mut server,&client,json!({"name":"batches/job","done":true,"metadata":{"state":state},"error":{"code":code,"message":"not-public"}})).await.unwrap();
        assert_eq!(job.state, expected);
        assert!(job.state.terminal());
        assert_eq!(job.error_code, Some(code));
        assert!(job.output.is_none());
    }
    let rows = json!([{"metadata":{"key":"a"},"response":{"usageMetadata":{"promptTokenCount":0}}},{"metadata":{"key":"b"},"error":{"code":13}}]);
    let job=gemini_job(&mut server,&client,json!({"name":"batches/job","state":"JOB_STATE_SUCCEEDED","dest":{"inlinedResponses":rows}})).await.unwrap();
    assert_eq!(
        job.output,
        Some(Output::Inline(rows.as_array().unwrap().clone()))
    );
    for value in [
        json!({"name":"batches/job","done":false,"response":{}}),
        json!({"name":"batches/job","done":true,"response":{"inlinedResponses":{"inlinedResponses":[]}}}),
        json!({"name":"batches/job","done":true,"response":{"inlinedResponses":{"inlinedResponses":[5]}}}),
    ] {
        assert!(gemini_job(&mut server, &client, value).await.is_err());
    }
}

#[tokio::test]
async fn vertex_rejects_wrong_job_acknowledgement_and_contradictory_errors() {
    let mut server = Server::new().await;
    let client = vertex(&server);
    let store = files(&server);
    let c = client.clone();
    let f = store.clone();
    let call = tokio::spawn(async move { c.create("image", "pictures", &f).await });
    server.next().await.json(json!({"name":"projects/elsewhere/locations/us-central1/batchPredictionJobs/a","state":"JOB_STATE_RUNNING"}));
    assert!(call.await.unwrap().unwrap_err().may_have_executed);
    for value in [
        json!({"name":job_name(),"state":"JOB_STATE_SUCCEEDED","error":{"code":13}}),
        json!({"name":job_name(),"state":"JOB_STATE_RUNNING","error":{"code":13}}),
        json!({"name":job_name(),"state":"UNKNOWN"}),
        json!({"name":job_name(),"state":"JOB_STATE_FAILED","error":{"code":"13"}}),
    ] {
        let c = client.clone();
        let f = store.clone();
        let call = tokio::spawn(async move { c.get(&job_name(), &f).await });
        server.next().await.json(value);
        assert!(call.await.unwrap().is_err());
    }
    let c = client.clone();
    let f = store.clone();
    let call = tokio::spawn(async move { c.get(&job_name(), &f).await });
    server.next().await.json(json!({"name":job_name(),"state":"JOB_STATE_FAILED","error":{"code":13,"message":"secret"}}));
    let job = call.await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert!(!format!("{job:?}").contains("secret"));
}

#[tokio::test]
async fn gcs_conflicting_input_is_never_overwritten_and_bad_upload_acks_stay_uncertain() {
    let mut server = Server::new().await;
    let store = files(&server);
    let key = format!("okapi-batches/{ID}/input.jsonl");
    let f = store.clone();
    let call = tokio::spawn(async move { f.upload_input(Bytes::from_static(b"abc")).await });
    server.next().await.respond(Reply::empty(412));
    server.next().await.json(object_json(&key, "7"));
    server.next().await.respond(Reply {
        status: 200,
        headers: vec![],
        chunks: vec!["xyz".into()],
        chunked: false,
    });
    assert_eq!(
        call.await.unwrap().unwrap_err().code,
        "batch_input_conflict"
    );
    server.quiet().await;
    for value in [
        object_json(&key, "0"),
        object_json(&key, "bad"),
        json!({"name":key,"bucket":"another-bucket","generation":"7"}),
        object_json(&format!("okapi-batches/{ID}/output/other.jsonl"), "7"),
    ] {
        let f = store.clone();
        let call = tokio::spawn(async move { f.upload_input(Bytes::from_static(b"abc")).await });
        server.next().await.json(value);
        assert!(call.await.unwrap().unwrap_err().may_have_executed);
    }
}

#[tokio::test]
async fn gcs_pagination_refuses_cycles_and_duplicate_live_versions() {
    let mut server = Server::new().await;
    let store = files(&server);
    let key = format!("okapi-batches/{ID}/output/a.jsonl");
    for response in [
        json!({"nextPageToken":"current"}),
        json!({"items":[object_json(&key,"1"),object_json(&key,"1")]}),
        json!({"items":[object_json(&key,"1"),object_json(&key,"2")]}),
        json!({"items":{}}),
    ] {
        let f = store.clone();
        let call =
            tokio::spawn(async move { f.list_output(Some("current"), Versions::Live).await });
        server.next().await.json(response);
        assert!(call.await.unwrap().is_err());
    }
    let f = store.clone();
    let call = tokio::spawn(async move { f.list_output(None, Versions::All).await });
    server
        .next()
        .await
        .json(json!({"items":[object_json(&key,"1"),object_json(&key,"2")]}));
    assert_eq!(call.await.unwrap().unwrap().objects.len(), 2);
}

#[tokio::test]
async fn jsonl_declared_overflow_and_truncated_transfers_never_look_like_clean_eof() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let c = client.clone();
    let call = tokio::spawn(async move { c.download("files/output", result_limits()).await });
    let mut reply = Reply::empty(200);
    reply.headers.push(("content-length", "4097".into()));
    server.next().await.respond(reply);
    let Err(error) = call.await.unwrap() else {
        panic!("oversized response accepted")
    };
    assert_eq!(error.code, "batch_result_size");
    let c = client.clone();
    let call = tokio::spawn(async move { c.download("files/output", result_limits()).await });
    server.next().await.respond(Reply {
        status: 200,
        headers: vec![("content-length", "100".into())],
        chunks: vec!["{}\n".into()],
        chunked: false,
    });
    let mut reader = call.await.unwrap().unwrap();
    let error = loop {
        match reader.next().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("truncated stream passed"),
            Err(e) => break e,
        }
    };
    assert_eq!(error.code, "batch_result_transport");
    assert!(reader.next().await.unwrap().is_none());
}
