use super::*;

fn gemini_identity() -> Value {
    json!({"name":"batches/job","metadata":{"displayName":"submit-label","model":"models/image-model","inputConfig":{"fileName":"files/input"},"state":"JOB_STATE_RUNNING"}})
}
fn query(path: &str) -> std::collections::HashMap<String, String> {
    reqwest::Url::parse(&format!("http://peer{path}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[tokio::test]
async fn gemini_lookup_pages_are_account_scoped_and_preserve_conflicting_evidence() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let c = client.clone();
    let call = tokio::spawn(async move {
        c.lookup_page(
            "submit-label",
            "image-model",
            "files/input",
            Some("a+/?&=b"),
        )
        .await
    });
    let req = server.next().await;
    assert_eq!(req.method, "GET");
    assert!(req.path.starts_with("/proxy/v1beta/batches?"));
    assert_eq!(req.headers["x-goog-api-key"], "gemini-secret");
    let params = query(&req.path);
    assert_eq!(params["pageSize"], "100");
    assert_eq!(params["pageToken"], "a+/?&=b");
    assert!(!params.contains_key("filter"));
    let mut wrong = gemini_identity();
    wrong["metadata"]["inputConfig"]["fileName"] = json!("files/somebody-else");
    req.json(json!({"operations":[gemini_identity(),wrong,{"name":"batches/second","metadata":{"displayName":"submit-label"}},{"name":"batches/unrelated","metadata":{"displayName":"other-label"}}],"nextPageToken":"next"}));
    let page = call.await.unwrap().unwrap();
    assert_eq!(page.names, ["batches/job", "batches/second"]);
    assert_eq!(page.next_page.as_deref(), Some("next"));
    assert!(page.conflict);
    server.quiet().await;
}

#[tokio::test]
async fn gemini_verified_lookup_requires_all_identity_fields_and_consistent_aliases() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let good = gemini_identity();
    let mut cases = vec![(good.clone(), true)];
    for pointer in [
        "/metadata/displayName",
        "/metadata/model",
        "/metadata/inputConfig/fileName",
        "/name",
    ] {
        let mut row = good.clone();
        *row.pointer_mut(pointer).unwrap() = json!("wrong");
        cases.push((row, false));
    }
    let mut missing = good.clone();
    missing["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("inputConfig");
    cases.push((missing, false));
    let mut alias = good;
    alias["model"] = json!("different-model");
    cases.push((alias, false));
    for (response, valid) in cases {
        let c = client.clone();
        let call = tokio::spawn(async move {
            c.get_verified("batches/job", "submit-label", "image-model", "files/input")
                .await
        });
        let req = server.next().await;
        assert_eq!(req.path, "/proxy/v1beta/batches/job");
        req.json(response);
        assert_eq!(call.await.unwrap().is_ok(), valid);
    }
}

#[tokio::test]
async fn lookup_rejects_partial_malformed_oversized_and_cyclic_pages() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    for response in [
        json!(null),
        json!([]),
        json!({"operations":{}}),
        json!({"operations":[null]}),
        json!({"unreachable":["one-region"]}),
        json!({"operations":vec![gemini_identity();101]}),
        json!({"nextPageToken":42}),
        json!({"nextPageToken":"same"}),
        json!({"nextPageToken":"x".repeat(4097)}),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move {
            c.lookup_page("submit-label", "image-model", "files/input", Some("same"))
                .await
        });
        server.next().await.json(response);
        assert!(call.await.unwrap().is_err());
    }
    for cursor in ["", "line\nbreak"] {
        assert!(
            client
                .lookup_page("submit-label", "image-model", "files/input", Some(cursor))
                .await
                .is_err()
        );
    }
    server.quiet().await;
}

fn vertex_identity(files: &GcsStore) -> Value {
    json!({"name":job_name(),"displayName":"submit-label","model":"publishers/google/models/image-model","inputConfig":{"instancesFormat":"jsonl","gcsSource":{"uris":[files.input_uri()]}},"instanceConfig":{"keyField":"key"},"state":"JOB_STATE_RUNNING"})
}

#[tokio::test]
async fn vertex_lookup_filters_exact_display_and_rejects_wrong_parent_model_or_input() {
    let mut server = Server::new().await;
    let client = vertex(&server);
    let files = files(&server);
    let good = vertex_identity(&files);
    let mut cases = vec![(good.clone(), false)];
    for pointer in [
        "/name",
        "/model",
        "/inputConfig/gcsSource/uris/0",
        "/instanceConfig/keyField",
    ] {
        let mut row = good.clone();
        *row.pointer_mut(pointer).unwrap() = json!("wrong");
        cases.push((row, true));
    }
    for (response, conflict) in cases {
        let c = client.clone();
        let f = files.clone();
        let call =
            tokio::spawn(
                async move { c.lookup_page("submit-label", "image-model", &f, None).await },
            );
        let req = server.next().await;
        assert_eq!(req.method, "GET");
        assert!(req.path.starts_with(
            "/proxy/v1/projects/project-a/locations/us-central1/batchPredictionJobs?"
        ));
        assert_eq!(req.headers["authorization"], "Bearer vertex-secret");
        let params = query(&req.path);
        assert_eq!(params["filter"], "displayName=\"submit-label\"");
        assert_eq!(params["pageSize"], "100");
        req.json(json!({"batchPredictionJobs":[response]}));
        let page = call.await.unwrap().unwrap();
        assert_eq!(page.conflict, conflict);
        assert_eq!(page.names.len(), usize::from(!conflict));
    }
}

#[tokio::test]
async fn vertex_verified_get_accepts_regional_model_but_requires_exact_input() {
    let mut server = Server::new().await;
    let client = vertex(&server);
    let files = files(&server);
    let mut good = vertex_identity(&files);
    good["model"] =
        json!("projects/project-a/locations/us-central1/publishers/google/models/image-model");
    let mut missing = good.clone();
    missing.as_object_mut().unwrap().remove("inputConfig");
    let mut multiple = good.clone();
    multiple["inputConfig"]["gcsSource"]["uris"] = json!([files.input_uri(), "gs://other/input"]);
    for (row, valid) in [(good, true), (missing, false), (multiple, false)] {
        let c = client.clone();
        let f = files.clone();
        let call = tokio::spawn(async move {
            c.get_verified(&job_name(), "submit-label", "image-model", &f)
                .await
        });
        server.next().await.json(row);
        assert_eq!(call.await.unwrap().is_ok(), valid);
    }
    server.quiet().await;
}

#[tokio::test]
async fn gemini_operation_metadata_output_is_used_and_conflicting_results_are_rejected() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    for (state, error) in [
        ("JOB_STATE_SUCCEEDED", None),
        ("JOB_STATE_CANCELLED", Some(1)),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.get("batches/job").await });
        let mut response = json!({"name":"batches/job","done":true,"metadata":{"state":state,"output":{"responsesFile":"files/result"}}});
        if let Some(code) = error {
            response["error"] = json!({"code":code});
        }
        server.next().await.json(response);
        let job = call
            .await
            .unwrap()
            .expect("official metadata.output must be readable");
        assert_eq!(job.output, Some(Output::File("files/result".into())));
        assert_eq!(job.error_code, error);
    }
    for response in [
        json!({"name":"batches/job","done":true,"metadata":{"state":"JOB_STATE_SUCCEEDED","output":{"responsesFile":"files/a"}},"response":{"responsesFile":"files/b"}}),
        json!({"name":"batches/job","done":true,"metadata":{"state":"JOB_STATE_CANCELLED","output":{"responsesFile":"files/a"}},"response":{"responsesFile":"files/a"},"error":{"code":1}}),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.get("batches/job").await });
        server.next().await.json(response);
        assert!(call.await.unwrap().is_err());
    }
}
