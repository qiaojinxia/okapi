use super::*;

#[tokio::test]
async fn gcs_collection_list_uses_exact_reported_directory_and_rejects_siblings() {
    let mut server = Server::new().await;
    let store = files(&server);
    let directory = format!("{}prediction-job", store.output_uri());
    let prefix = format!("okapi-batches/{ID}/output/prediction-job/");
    for (key, valid) in [
        (format!("{prefix}predictions.jsonl"), true),
        (
            format!("okapi-batches/{ID}/output/prediction-job-sibling/predictions.jsonl"),
            false,
        ),
        (format!("okapi-batches/{ID}/output/other.jsonl"), false),
    ] {
        let client = store.clone();
        let dir = directory.clone();
        let call = tokio::spawn(async move { client.list_results(&dir, None).await });
        let req = server.next().await;
        let url = reqwest::Url::parse(&format!("http://peer{}", req.path)).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["prefix"], prefix);
        assert_eq!(query["versions"], "false");
        req.json(json!({"items":[object_json(&key,"7")]}));
        assert_eq!(call.await.unwrap().is_ok(), valid);
    }
    for uri in [
        "gs://other-bucket/output/",
        "gs://batch-bucket/other-task/output/",
    ] {
        assert!(store.list_results(uri, None).await.is_err());
    }
    // GCS object names are opaque: normalizing double slashes would widen scope.
    let c = store.clone();
    let dir = format!("{directory}//");
    let call = tokio::spawn(async move { c.list_results(&dir, None).await });
    let req = server.next().await;
    let url = reqwest::Url::parse(&format!("http://peer{}", req.path)).unwrap();
    assert_eq!(
        url.query_pairs().find(|(k, _)| k == "prefix").unwrap().1,
        format!("{prefix}/")
    );
    req.json(json!({"items":[object_json(&format!("{prefix}outside-double-slash.jsonl"),"7")]}));
    assert!(call.await.unwrap().is_err());
    assert!(
        store
            .list_results(&directory, Some("bad\ncursor"))
            .await
            .is_err()
    );
    server.quiet().await;
}

#[tokio::test]
async fn streamed_result_budget_counts_whitespace_and_does_not_reset_between_files() {
    let mut server = Server::new().await;
    let client = gemini(&server);
    let mut remaining = 64;
    for body in [
        b"   {\"key\":\"a\"}\r\n \n".as_slice(),
        b"{\"key\":\"b\"}\n   ".as_slice(),
    ] {
        let c = client.clone();
        let call = tokio::spawn(async move {
            c.download(
                "files/results",
                Limits {
                    line_bytes: remaining,
                    total_bytes: remaining,
                    rows: 2,
                },
            )
            .await
        });
        server.next().await.respond(Reply {
            status: 200,
            headers: vec![],
            chunks: vec![
                Bytes::copy_from_slice(&body[..3]),
                Bytes::copy_from_slice(&body[3..]),
            ],
            chunked: true,
        });
        let mut reader = call.await.unwrap().unwrap();
        assert!(reader.next().await.unwrap().is_some());
        assert!(reader.next().await.unwrap().is_none());
        assert_eq!(reader.bytes_read(), body.len());
        remaining -= reader.bytes_read();
    }
    let c = client.clone();
    let call = tokio::spawn(async move {
        c.download(
            "files/results",
            Limits {
                line_bytes: remaining,
                total_bytes: remaining,
                rows: 1,
            },
        )
        .await
    });
    server.next().await.respond(Reply {
        status: 200,
        headers: vec![],
        chunks: vec![Bytes::from(vec![b' '; remaining + 1])],
        chunked: true,
    });
    let mut reader = call.await.unwrap().unwrap();
    assert!(reader.next().await.is_err());
    assert!(
        client
            .download(
                "files/results",
                Limits {
                    line_bytes: 1,
                    total_bytes: jsonl::MAX_RESULT_BYTES + 1,
                    rows: 1
                }
            )
            .await
            .is_err()
    );
    server.quiet().await;
}
