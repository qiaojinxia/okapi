use super::*;

#[tokio::test]
async fn vertex_delete_tracks_async_operation_and_never_interprets_errors_as_absence() {
    let mut server = Server::new().await;
    let client = vertex(&server);
    let name = "projects/project-a/locations/us-central1/operations/delete-1";
    let c = client.clone();
    let call = tokio::spawn(async move { c.delete_job(&job_name()).await });
    let req = server.next().await;
    assert_eq!(req.method, "DELETE");
    assert_eq!(req.path, format!("/proxy/v1/{}", job_name()));
    assert!(req.body.is_empty());
    assert_eq!(req.headers["authorization"], "Bearer vertex-secret");
    req.json(json!({"name":name}));
    let operation = call.await.unwrap().unwrap().unwrap();
    assert_eq!(operation.name, name);
    assert!(!operation.done);
    for (value, done, failed) in [
        (json!({"name":name,"done":false}), false, false),
        (json!({"name":name,"done":true,"response":{}}), true, false),
        (
            json!({"name":name,"done":true,"error":{"code":7,"message":"private"}}),
            true,
            true,
        ),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.deletion(&job_name(), name).await });
        let req = server.next().await;
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, format!("/proxy/v1/{name}"));
        req.json(value);
        let operation = call.await.unwrap().unwrap().unwrap();
        assert_eq!((operation.done, operation.failed), (done, failed));
    }
    for status in [404, 403, 503] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.delete_job(&job_name()).await });
        server.next().await.respond(Reply::empty(status));
        let result = call.await.unwrap();
        if status == 404 {
            assert!(result.unwrap().is_none());
        } else {
            assert_eq!(result.err().unwrap().status, Some(status));
        }
    }
    server.quiet().await;
}

#[tokio::test]
async fn vertex_delete_rejects_foreign_operations_and_malformed_completion() {
    let mut server = Server::new().await;
    let client = vertex(&server);
    let name = "projects/project-a/locations/us-central1/operations/delete-1";
    for value in [
        json!({"name":"projects/other/locations/us-central1/operations/delete-1"}),
        json!({"name":name,"done":"true"}),
        json!({"name":name,"done":false,"response":{}}),
        json!({"name":name,"done":false,"error":{"code":7}}),
        json!({"name":name,"done":true,"error":{"code":0}}),
        json!({"name":name,"done":true,"error":{"code":7},"response":{}}),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.delete_job(&job_name()).await });
        server.next().await.json(value);
        assert!(call.await.unwrap().is_err());
    }
    for name in [
        "https://foreign/operations/a",
        "projects/project-a/locations/elsewhere/operations/x",
        "projects/project-a/locations/us-central1/batchPredictionJobs/other/operations/x",
    ] {
        assert!(client.deletion(&job_name(), name).await.is_err());
    }
    let c = client.clone();
    let call = tokio::spawn(async move { c.deletion(&job_name(), name).await });
    server.next().await.json(
        json!({"name":"projects/project-a/locations/us-central1/operations/other","done":true}),
    );
    assert!(call.await.unwrap().is_err());
    server.quiet().await;
}

#[tokio::test]
async fn gcs_cleanup_lists_all_input_output_versions_and_rejects_foreign_objects() {
    let mut server = Server::new().await;
    let client = files(&server);
    let input = format!("okapi-batches/{ID}/input.jsonl");
    let output = format!("okapi-batches/{ID}/output/result.jsonl");
    let c = client.clone();
    let call = tokio::spawn(async move { c.list_cleanup(None).await });
    let req = server.next().await;
    let url = reqwest::Url::parse(&format!("http://peer{}", req.path)).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["versions"], "true");
    assert_eq!(query["prefix"], format!("okapi-batches/{ID}/"));
    req.json(json!({"items":[object_json(&input,"1"),object_json(&input,"2"),object_json(&output,"3")],"nextPageToken":"page-2"}));
    let page = call.await.unwrap().unwrap();
    assert_eq!(page.objects.len(), 3);
    assert_eq!(page.next_page.as_deref(), Some("page-2"));
    for key in [
        format!("okapi-batches/{ID}/input.jsonl.extra"),
        "okapi-batches/foreign/output/result.jsonl".into(),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move { c.list_cleanup(Some("page-2")).await });
        server
            .next()
            .await
            .json(json!({"items":[object_json(&key,"9")]}));
        assert!(call.await.unwrap().is_err());
    }
    server.quiet().await;
}
