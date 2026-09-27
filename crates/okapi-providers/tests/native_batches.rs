#[path = "support/batch_cleanup.rs"]
mod cleanup;
#[path = "support/batch_edges.rs"]
mod edges;
#[path = "support/batch_peer.rs"]
mod peer;
#[path = "support/batch_recovery.rs"]
mod recovery;
#[path = "support/batch_results.rs"]
mod results;
use bytes::Bytes;
use okapi_providers::{
    batch::{
        JobState, Output, Request,
        gemini::{FileState, GeminiBatch},
        jsonl::{self, Limits},
        vertex::{
            VertexBatch,
            gcs::{GcsStore, Object, Versions},
        },
    },
    http::Outbound,
};
use peer::{Reply, Server};
use serde_json::{Value, json};

const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn gemini(server: &Server) -> GeminiBatch {
    GeminiBatch::new(
        &format!("{}/proxy/v1beta", server.base),
        "gemini-secret",
        &Outbound::default(),
    )
    .unwrap()
}
fn files(server: &Server) -> GcsStore {
    GcsStore::new(
        &format!("{}/proxy", server.base),
        "gcs-secret",
        &Outbound::default(),
        "batch-bucket",
        ID,
    )
    .unwrap()
}
fn vertex(server: &Server) -> VertexBatch {
    VertexBatch::new(
        &format!(
            "{}/proxy/v1/projects/project-a/locations/us-central1",
            server.base
        ),
        "vertex-secret",
        &Outbound::default(),
    )
    .unwrap()
}
fn requests() -> Vec<Request> {
    vec![Request {
        key: "item-a".into(),
        request: json!({"contents":[{"role":"user","parts":[{"text":"生成猫"}]}],"generationConfig":{"responseModalities":["TEXT","IMAGE"],"imageConfig":{"imageSize":"2K"}}}),
    }]
}
fn job_name() -> String {
    "projects/project-a/locations/us-central1/batchPredictionJobs/job-1".into()
}
fn object_json(key: &str, generation: &str) -> Value {
    json!({"name":key,"bucket":"batch-bucket","generation":generation})
}
fn result_limits() -> Limits {
    Limits {
        line_bytes: 1024,
        total_bytes: 4096,
        rows: 10,
    }
}

#[tokio::test]
async fn gemini_inline_posts_once_and_reads_rest_operation_inline_results() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let c = client.clone();
    let call = tokio::spawn(async move {
        c.create_inline("models/gemini-image", "pictures", &requests())
            .await
    });
    let req = server.next().await;
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.path,
        "/proxy/v1beta/models/gemini-image:batchGenerateContent"
    );
    assert_eq!(req.headers["x-goog-api-key"], "gemini-secret");
    assert!(!req.headers.contains_key("authorization"));
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(
        body["batch"]["inputConfig"]["requests"]["requests"][0]["request"],
        requests()[0].request
    );
    assert_eq!(
        body["batch"]["inputConfig"]["requests"]["requests"][0]["metadata"]["key"],
        "item-a"
    );
    req.json(
        json!({"name":"batches/job-1","metadata":{"state":"BATCH_STATE_PENDING"},"done":false}),
    );
    assert_eq!(call.await.unwrap().unwrap().state, JobState::Pending);
    let c = client.clone();
    let call = tokio::spawn(async move { c.get("batches/job-1").await });
    let req = server.next().await;
    assert_eq!(req.method, "GET");
    let rows = json!([{"metadata":{"key":"item-a"},"response":{"candidates":[{"content":{"parts":[{"inlineData":{"mimeType":"image/png","data":"aW1hZ2U="}}]}}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20}}}]);
    req.json(json!({"name":"batches/job-1","done":true,"metadata":{"state":"BATCH_STATE_SUCCEEDED"},"response":{"inlinedResponses":{"inlinedResponses":rows}}}));
    let job = call.await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        job.output,
        Some(Output::Inline(rows.as_array().unwrap().clone()))
    );
    server.quiet().await;
}

fn restored_upload(
    client: &GeminiBatch,
    encoded: &str,
    size: usize,
) -> okapi_providers::batch::gemini::UploadSession {
    assert!(client.restore_upload(encoded, "files/other", size).is_err());
    assert!(
        client
            .restore_upload(encoded, "files/input-1", size + 1)
            .is_err()
    );
    let mut forged: Value = serde_json::from_str(encoded).unwrap();
    forged["url"] = json!("https://elsewhere.invalid/upload/v1beta/files?secret=private");
    assert!(
        client
            .restore_upload(&forged.to_string(), "files/input-1", size)
            .is_err()
    );
    client
        .restore_upload(encoded, "files/input-1", size)
        .unwrap()
}

#[tokio::test]
async fn gemini_resumable_file_upload_poll_create_download_and_cleanup() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let data = jsonl::encode(&requests()).unwrap();
    let size = data.len();
    let c = client.clone();
    let call = tokio::spawn(async move { c.start_upload("files/input-1", "pictures", size).await });
    let req = server.next().await;
    assert_eq!(req.path, "/proxy/upload/v1beta/files");
    assert_eq!(req.headers["x-goog-upload-protocol"], "resumable");
    assert_eq!(
        req.headers["x-goog-upload-header-content-length"],
        size.to_string()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&req.body).unwrap()["file"]["name"],
        "files/input-1"
    );
    let mut reply = Reply::empty(200);
    reply.headers.push((
        "x-goog-upload-url",
        format!(
            "{}/proxy/upload/v1beta/files?upload_id=private-session",
            server.base
        ),
    ));
    req.respond(reply);
    let session = call.await.unwrap().unwrap();
    let session = restored_upload(&client, &session.encode(), size);
    let c = client.clone();
    let bytes = data.clone();
    let call = tokio::spawn(async move { c.finish_upload(&session, bytes).await });
    let req = server.next().await;
    assert_eq!(req.headers["x-goog-upload-command"], "upload, finalize");
    assert_eq!(req.body, data);
    req.json(json!({"file":{"name":"files/input-1","state":"PROCESSING"}}));
    let uploaded = call.await.unwrap().unwrap();
    assert_eq!(uploaded.state, FileState::Processing);
    assert!(
        client
            .create_file("gemini-image", "pictures", &uploaded)
            .await
            .is_err()
    );
    let c = client.clone();
    let call = tokio::spawn(async move { c.file("files/input-1").await });
    server
        .next()
        .await
        .json(json!({"name":"files/input-1","state":"ACTIVE"}));
    let uploaded = call.await.unwrap().unwrap();
    let c = client.clone();
    let call =
        tokio::spawn(async move { c.create_file("gemini-image", "pictures", &uploaded).await });
    let req = server.next().await;
    assert_eq!(
        serde_json::from_slice::<Value>(&req.body).unwrap()["batch"]["inputConfig"],
        json!({"fileName":"files/input-1"})
    );
    req.json(json!({"name":"batches/file-job","state":"JOB_STATE_SUCCEEDED","dest":{"fileName":"files/output-1"}}));
    assert_eq!(
        call.await.unwrap().unwrap().output,
        Some(Output::File("files/output-1".into()))
    );
    let c = client.clone();
    let call = tokio::spawn(async move { c.download("files/output-1", result_limits()).await });
    let req = server.next().await;
    assert_eq!(
        req.path,
        "/proxy/download/v1beta/files/output-1:download?alt=media"
    );
    req.respond(Reply {
        status: 200,
        headers: vec![],
        chunks: vec![Bytes::from_static(
            b"{\"key\":\"item-a\",\"response\":{} }\n",
        )],
        chunked: true,
    });
    let mut reader = call.await.unwrap().unwrap();
    assert_eq!(reader.next().await.unwrap().unwrap()["key"], "item-a");
    assert!(reader.next().await.unwrap().is_none());
    for name in ["files/input-1", "files/output-1"] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.delete_file(name).await });
        let req = server.next().await;
        assert_eq!(req.method, "DELETE");
        assert_eq!(req.path, format!("/proxy/v1beta/{name}"));
        req.respond(Reply::empty(404));
        call.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn cancel_is_only_acknowledged_and_later_success_is_preserved() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let c = client.clone();
    let call = tokio::spawn(async move { c.cancel("batches/job").await });
    let req = server.next().await;
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/proxy/v1beta/batches/job:cancel");
    assert!(req.body.is_empty());
    req.json(json!({}));
    call.await.unwrap().unwrap();
    let c = client.clone();
    let call = tokio::spawn(async move { c.get("batches/job").await });
    server.next().await.json(
        json!({"name":"batches/job","done":true,"response":{"responsesFile":"files/result"}}),
    );
    assert_eq!(call.await.unwrap().unwrap().state, JobState::Succeeded);
    let c = client.clone();
    let call = tokio::spawn(async move { c.get("batches/job").await });
    server.next().await.json(json!({"name":"batches/job","done":true,"error":{"code":1,"message":"private-upstream-detail"}}));
    let job = call.await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Cancelled);
    assert!(!format!("{job:?}").contains("private-upstream-detail"));
    let c = client.clone();
    let call = tokio::spawn(async move { c.delete_job("batches/job").await });
    let req = server.next().await;
    assert_eq!(req.method, "DELETE");
    req.json(json!({}));
    call.await.unwrap().unwrap();
}

#[tokio::test]
async fn create_failures_are_bounded_sanitized_and_never_retried_or_redirected() {
    let mut server = Server::new().await;
    let mut trap = Server::new().await;
    let client = gemini(&server);
    for status in [302, 307, 308, 400, 401, 403, 408, 409, 429, 500, 503] {
        let c = client.clone();
        let call =
            tokio::spawn(async move { c.create_inline("image", "pictures", &requests()).await });
        let req = server.next().await;
        let mut reply = Reply::empty(status);
        reply.headers = vec![
            ("location", format!("{}/stolen", trap.base)),
            ("retry-after", "7".into()),
        ];
        reply.chunks.push("credential=should-not-leak".into());
        req.respond(reply);
        let err = call.await.unwrap().unwrap_err();
        assert_eq!(err.status, Some(status));
        assert_eq!(err.retry_after_secs, Some(7));
        assert_eq!(
            err.may_have_executed,
            !(400..500).contains(&status) || status == 408
        );
        assert!(!format!("{err:?}").contains("should-not-leak"));
        server.quiet().await;
    }
    trap.quiet().await;
    let c = client.clone();
    let call = tokio::spawn(async move { c.create_inline("image", "pictures", &requests()).await });
    drop(server.next().await);
    assert!(call.await.unwrap().unwrap_err().may_have_executed);
    server.quiet().await;
    for reply in [
        Reply {
            status: 200,
            headers: vec![],
            chunks: vec!["{broken-private-json".into()],
            chunked: false,
        },
        Reply {
            status: 200,
            headers: vec![("content-length", (64 * 1024 * 1024 + 1).to_string())],
            chunks: vec![],
            chunked: false,
        },
    ] {
        let c = client.clone();
        let call =
            tokio::spawn(async move { c.create_inline("image", "pictures", &requests()).await });
        server.next().await.respond(reply);
        assert!(call.await.unwrap().unwrap_err().may_have_executed);
    }
}

#[tokio::test]
async fn invalid_resources_and_untrusted_upload_locations_do_not_receive_credentials() {
    let mut server = Server::new().await;
    let mut trap = Server::new().await;
    let client = gemini(&server);
    for name in [
        "https://evil.test/job",
        "batches/../job",
        "batches/a?key=x",
        "batches/a#fragment",
        "batches/%2fsecret",
        "batches/a/extra",
        "files/..",
    ] {
        assert!(client.get(name).await.is_err());
        assert!(client.cancel(name).await.is_err());
        assert!(client.delete_job(name).await.is_err());
    }
    for location in [
        format!("{}/proxy/upload/v1beta/files?secret=session", trap.base),
        format!("{}/other?session=x", server.base),
        format!("{}/proxy/upload/v1beta/files#fragment", server.base),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.start_upload("files/input", "pictures", 3).await });
        let mut reply = Reply::empty(200);
        reply.headers.push(("x-goog-upload-url", location));
        server.next().await.respond(reply);
        let Err(err) = call.await.unwrap() else {
            panic!("untrusted upload accepted")
        };
        assert!(err.may_have_executed);
        assert_eq!(err.code, "batch_upload_url");
    }
    server.quiet().await;
    trap.quiet().await;
}

#[tokio::test]
async fn malformed_job_states_and_cross_job_results_are_rejected() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    for value in [
        json!({"name":"batches/other","state":"JOB_STATE_RUNNING"}),
        json!({"name":"batches/job","state":"NEW_STATE"}),
        json!({"name":"batches/job","done":"false"}),
        json!({"name":"batches/job","done":false,"metadata":{"state":"JOB_STATE_SUCCEEDED"}}),
        json!({"name":"batches/job","state":"JOB_STATE_FAILED","metadata":{"state":"JOB_STATE_SUCCEEDED"}}),
        json!({"name":"batches/job","done":true}),
        json!({"name":"batches/job","done":true,"error":{"code":"1"}}),
        json!({"name":"batches/job","done":true,"error":{"code":1},"response":{"responsesFile":"files/out"}}),
        json!({"name":"batches/job","done":true,"response":{"responsesFile":"https://evil.test/file"}}),
        json!({"name":"batches/job","done":true,"response":{"responsesFile":"files/a"},"dest":{"fileName":"files/b"}}),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.get("batches/job").await });
        server.next().await.json(value);
        assert!(call.await.unwrap().is_err());
    }
}

#[tokio::test]
async fn jsonl_stream_preserves_rows_across_chunks_and_rejects_all_capacity_overflows() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let c = client.clone();
    let call = tokio::spawn(async move { c.download("files/results", result_limits()).await });
    server.next().await.respond(Reply {
        status: 200,
        headers: vec![],
        chunked: true,
        chunks: vec![
            Bytes::from_static(b"\r\n{\"key\":\"a\",\"response\":{}}\r"),
            Bytes::from_static(b"\n{\"key\":\"b\",\"error\":{\"code\":13}}"),
        ],
    });
    let mut reader = call.await.unwrap().unwrap();
    assert_eq!(reader.next().await.unwrap().unwrap()["key"], "a");
    assert_eq!(reader.next().await.unwrap().unwrap()["error"]["code"], 13);
    assert!(reader.next().await.unwrap().is_none());
    let cases = [
        (
            Limits {
                line_bytes: 8,
                total_bytes: 32,
                rows: 3,
            },
            "{\"key\":\"too long\"}",
            "batch_result_line_size",
        ),
        (
            Limits {
                line_bytes: 8,
                total_bytes: 8,
                rows: 3,
            },
            "{}\n{}\n{}\n",
            "batch_result_size",
        ),
        (
            Limits {
                line_bytes: 32,
                total_bytes: 64,
                rows: 1,
            },
            "{}\n{}\n",
            "batch_result_rows",
        ),
        (result_limits(), "{broken}", "batch_result_json"),
        (result_limits(), "[]", "batch_result_json"),
    ];
    for (limits, body, code) in cases {
        let c = client.clone();
        let call = tokio::spawn(async move { c.download("files/results", limits).await });
        server.next().await.respond(Reply {
            status: 200,
            headers: vec![],
            chunked: true,
            chunks: vec![Bytes::from(body)],
        });
        let mut reader = call.await.unwrap().unwrap();
        let err = loop {
            match reader.next().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("invalid input passed"),
                Err(e) => break e,
            }
        };
        assert_eq!(err.code, code);
        assert!(reader.next().await.unwrap().is_none());
    }
}

#[test]
fn input_jsonl_has_unique_keys_and_preserves_provider_payloads() {
    let data = jsonl::encode(&requests()).unwrap();
    let row: Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(row["request"], requests()[0].request);
    assert_eq!(row["key"], "item-a");
    assert!(jsonl::encode(&[]).is_err());
    let mut duplicate = requests();
    duplicate.extend(requests());
    assert!(jsonl::encode(&duplicate).is_err());
    for key in ["", "bad\nkey"] {
        let mut rows = requests();
        rows[0].key = key.into();
        assert!(jsonl::encode(&rows).is_err());
    }
    let mut rows = requests();
    rows[0].request = json!({"contents":[]});
    assert!(jsonl::encode(&rows).is_err());
}

#[tokio::test]
async fn vertex_creates_native_job_with_scoped_gcs_and_preserves_partial_terminal_state() {
    let mut server = Server::new().await;
    let client = vertex(&server);
    let store = files(&server);
    let c = client.clone();
    let f = store.clone();
    let call = tokio::spawn(async move { c.create("gemini-image", "pictures", &f).await });
    let req = server.next().await;
    assert_eq!(
        req.path,
        "/proxy/v1/projects/project-a/locations/us-central1/batchPredictionJobs"
    );
    assert_eq!(req.headers["authorization"], "Bearer vertex-secret");
    assert!(!req.headers.contains_key("x-goog-api-key"));
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["model"], "publishers/google/models/gemini-image");
    assert_eq!(
        body["inputConfig"]["gcsSource"]["uris"][0],
        store.input_uri()
    );
    assert_eq!(body["instanceConfig"]["keyField"], "key");
    req.json(json!({"name":job_name(),"state":"JOB_STATE_PENDING"}));
    assert_eq!(call.await.unwrap().unwrap().state, JobState::Pending);
    let c = client.clone();
    let f = store.clone();
    let call = tokio::spawn(async move { c.get(&job_name(), &f).await });
    let req = server.next().await;
    let output = format!("{}prediction-001/", store.output_uri());
    req.json(json!({"name":job_name(),"state":"JOB_STATE_PARTIALLY_SUCCEEDED","outputInfo":{"gcsOutputDirectory":output}}));
    let job = call.await.unwrap().unwrap();
    assert_eq!(job.state, JobState::PartiallySucceeded);
    assert_eq!(job.output, Some(Output::GcsPrefix(output)));
    let c = client.clone();
    let call = tokio::spawn(async move { c.cancel(&job_name()).await });
    let req = server.next().await;
    assert_eq!(req.path, format!("/proxy/v1/{}:cancel", job_name()));
    req.json(json!({}));
    call.await.unwrap().unwrap();
}

#[tokio::test]
async fn gcs_input_is_conditional_and_acknowledgement_loss_rechecks_exact_version() {
    let mut server = Server::new().await;
    let store = files(&server);
    let data = jsonl::encode(&requests()).unwrap();
    let f = store.clone();
    let bytes = data.clone();
    let key = format!("okapi-batches/{ID}/input.jsonl");
    let call = tokio::spawn(async move { f.upload_input(bytes).await });
    let req = server.next().await;
    assert_eq!(req.headers["authorization"], "Bearer gcs-secret");
    assert_eq!(req.body, data);
    let url = reqwest::Url::parse(&format!("{}{}", server.base, req.path)).unwrap();
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "ifGenerationMatch" && v == "0")
    );
    req.json(object_json(&key, "123"));
    let saved = call.await.unwrap().unwrap();
    assert_eq!(saved.generation, "123");
    let f = store.clone();
    let bytes = data.clone();
    let call = tokio::spawn(async move { f.upload_input(bytes).await });
    server.next().await.respond(Reply::empty(412));
    let req = server.next().await;
    assert_eq!(req.method, "GET");
    req.json(object_json(&key, "123"));
    let req = server.next().await;
    assert!(req.path.contains("generation=123"));
    assert!(req.path.contains("alt=media"));
    req.respond(Reply {
        status: 200,
        headers: vec![],
        chunks: vec![data],
        chunked: false,
    });
    assert_eq!(call.await.unwrap().unwrap(), saved);
    let f = store.clone();
    let object = saved.clone();
    let call = tokio::spawn(async move { f.delete(&object).await });
    let req = server.next().await;
    assert_eq!(req.method, "DELETE");
    assert!(req.path.ends_with("?generation=123"));
    req.respond(Reply::empty(204));
    call.await.unwrap().unwrap();
}

#[tokio::test]
async fn gcs_list_download_and_delete_keep_prefix_versions_and_encoded_object_names() {
    let mut server = Server::new().await;
    let store = files(&server);
    let f = store.clone();
    let key = format!("okapi-batches/{ID}/output/图 +%?.jsonl");
    let call = tokio::spawn(async move { f.list_output(None, Versions::All).await });
    let req = server.next().await;
    assert!(req.path.contains("versions=true"));
    assert!(req.path.contains("maxResults=1000"));
    req.json(json!({"items":[object_json(&key,"5")],"nextPageToken":"opaque +/="}));
    let page = call.await.unwrap().unwrap();
    let object = page.objects[0].clone();
    let f = store.clone();
    let call = tokio::spawn(async move {
        f.list_output(page.next_page.as_deref(), Versions::All)
            .await
    });
    let req = server.next().await;
    let url = reqwest::Url::parse(&format!("{}{}", server.base, req.path)).unwrap();
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "pageToken" && v == "opaque +/=")
    );
    req.json(json!({"items":[]}));
    assert!(call.await.unwrap().unwrap().next_page.is_none());
    let f = store.clone();
    let obj = object.clone();
    let call = tokio::spawn(async move { f.download(&obj, result_limits()).await });
    let req = server.next().await;
    assert!(req.path.contains("%2Foutput%2F"));
    assert!(
        req.path
            .contains("%20%2B%25%3F.jsonl?generation=5&alt=media")
    );
    req.json(json!({"key":"item-a","response":{"usageMetadata":{"promptTokenCount":1}}}));
    let mut reader = call.await.unwrap().unwrap();
    assert_eq!(reader.next().await.unwrap().unwrap()["key"], "item-a");
    let f = store.clone();
    let call = tokio::spawn(async move { f.delete(&object).await });
    server.next().await.respond(Reply::empty(404));
    call.await.unwrap().unwrap();
}

#[tokio::test]
async fn vertex_and_gcs_reject_cross_project_prefix_and_generation_injection() {
    let mut server = Server::new().await;
    let client = vertex(&server);
    let store = files(&server);
    assert!(
        client
            .get(
                "projects/other/locations/us-central1/batchPredictionJobs/job",
                &store
            )
            .await
            .is_err()
    );
    for object in [
        Object {
            key: "other-user/output/x".into(),
            generation: "1".into(),
        },
        Object {
            key: format!("okapi-batches/{ID}/output/x"),
            generation: "1&generation=2".into(),
        },
    ] {
        assert!(store.delete(&object).await.is_err());
        assert!(store.download(&object, result_limits()).await.is_err());
    }
    for output in [
        "gs://another-bucket/output/".to_owned(),
        format!("{}../escape", store.output_uri()),
        format!("{}%2e%2e/escape", store.output_uri()),
    ] {
        let c = client.clone();
        let f = store.clone();
        let call = tokio::spawn(async move { c.get(&job_name(), &f).await });
        server.next().await.json(json!({"name":job_name(),"state":"JOB_STATE_SUCCEEDED","outputInfo":{"gcsOutputDirectory":output}}));
        assert!(call.await.unwrap().is_err());
    }
    let f = store.clone();
    let call = tokio::spawn(async move { f.list_output(None, Versions::Live).await });
    server
        .next()
        .await
        .json(json!({"items":[object_json("another-user/output/x","5")]}));
    assert!(call.await.unwrap().is_err());
    server.quiet().await;
}
