//! Real HTTP SSE + PG/Redis receipts, with controlled upstream chunk boundaries.
use super::token_billing::{assert_usage, token_env, usage};
use super::*;

#[path = "image_streaming_edges.rs"]
mod edges;

#[path = "image_streaming_cache.rs"]
mod cache;

type Chunks = mpsc::Sender<Result<Bytes, std::io::Error>>;

fn stream(peer: Pending) -> Chunks {
    let (tx, rx) = mpsc::channel(4);
    let body = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    });
    peer.reply
        .send(
            Response::builder()
                .header("content-type", "text/event-stream; charset=utf-8")
                .header("x-request-id", "image-sse-peer")
                .body(Body::from_stream(body))
                .unwrap(),
        )
        .unwrap();
    tx
}

async fn send(tx: &Chunks, frame: Value) {
    tx.send(Ok(Bytes::from(format!(
        "event: {}\ndata: {frame}\n\n",
        frame["type"].as_str().unwrap()
    ))))
    .await
    .unwrap();
}

fn preview(edit: bool) -> Value {
    json!({"type":if edit {"image_edit.partial_image"} else {"image_generation.partial_image"},
        "partial_image_index":0,"b64_json":"cHJldmlldw==","output_format":"png"})
}

fn completed(edit: bool, usage: Option<Value>) -> Value {
    let mut frame = json!({"type":if edit {"image_edit.completed"} else {"image_generation.completed"},
        "b64_json":"aW1hZ2U=","size":"1024x1024","quality":"high","created_at":1_700_000_000});
    if let Some(usage) = usage {
        frame["usage"] = usage;
    }
    frame
}

fn body(env: &Env, n: u32) -> Value {
    let mut body = env.body(n);
    body["stream"] = json!(true);
    body["partial_images"] = json!(1);
    body
}

async fn chunk(response: &mut reqwest::Response) -> String {
    String::from_utf8(
        timeout(WAIT, response.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn previews_arrive_before_finish_and_cumulative_usage_is_not_multiplied() {
    let mut env = token_env().await;
    let call = launch(env.request(false).json(&body(&env, 2)));
    let peer = env.peer().await;
    assert_eq!(peer.body["model"], "mapped-image");
    assert_eq!(peer.body["partial_images"], 1);
    assert_eq!(peer.headers["authorization"], "Bearer image-credential");
    let tx = stream(peer);
    send(&tx, preview(false)).await;
    let mut response = finish(call, 200).await;
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert!(
        chunk(&mut response)
            .await
            .contains("image_generation.partial_image")
    );
    let records: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE user_id=$1")
        .bind(env.user)
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(records, 0);
    send(&tx, completed(false, Some(usage(20, 80, 100)))).await;
    send(&tx, completed(false, Some(usage(20, 80, 200)))).await;
    tx.send(Ok(Bytes::from_static(b"data: [DONE]\n\n")))
        .await
        .unwrap();
    drop(tx);
    let record = env.record(&response).await;
    assert_usage(&env, &record, 20, 80, 200).await;
    assert_eq!(record["is_stream"], true);
    assert!(record["ttft_ms"].is_number());
    assert_eq!(record["upstream_request_id"], "image-sse-peer");
    assert_eq!(
        record["pricing_snapshot"]["image_stream_usage"],
        "cumulative"
    );
    assert_eq!(record["pricing_snapshot"]["image_stream_incomplete"], false);
    let text = response.text().await.unwrap();
    assert_eq!(text.matches("event: image_generation.completed").count(), 2);
    assert!(text.contains("[DONE]"));
    env.assert_money(6740, 1).await;
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn per_image_mode_sums_each_receipt_once() {
    let mut env = token_env().await;
    sqlx::query("UPDATE channels SET settings=jsonb_set(settings,'{image_stream_usage}','\"per_image\"') WHERE id=ANY($1)")
        .bind(&env.channels).execute(&env.state.pg).await.unwrap();
    let call = launch(env.request(false).json(&body(&env, 2)));
    let tx = stream(env.peer().await);
    send(&tx, completed(false, Some(usage(10, 40, 100)))).await;
    send(&tx, completed(false, Some(usage(10, 40, 100)))).await;
    drop(tx);
    let response = finish(call, 200).await;
    let record = env.record(&response).await;
    assert_usage(&env, &record, 20, 80, 200).await;
    assert_eq!(
        record["pricing_snapshot"]["image_stream_usage"],
        "per_image"
    );
    assert_eq!(
        response
            .text()
            .await
            .unwrap()
            .matches("event: image_generation.completed")
            .count(),
        2
    );
    env.assert_money(6740, 1).await;
}

#[tokio::test]
async fn disconnect_after_preview_still_drains_and_bills_completed_images() {
    let mut env = token_env().await;
    let call = launch(env.request(false).json(&body(&env, 1)));
    let tx = stream(env.peer().await);
    send(&tx, preview(false)).await;
    let mut response = finish(call, 200).await;
    assert!(chunk(&mut response).await.contains("partial_image"));
    drop(response);
    send(&tx, completed(false, Some(usage(20, 80, 200)))).await;
    drop(tx);
    env.assert_money(6740, 1).await;
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disconnect_while_waiting_for_headers_does_not_abandon_reservation() {
    let mut env = setup().await;
    let call = launch(env.request(false).json(&body(&env, 1)));
    let peer = env.peer().await;
    call.abort();
    let _ = call.await;
    let tx = stream(peer);
    send(&tx, completed(false, None)).await;
    drop(tx);
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn completed_prefix_is_billed_once_before_sanitized_upstream_error() {
    let mut env = token_env().await;
    let call = launch(env.request(false).json(&body(&env, 2)));
    let tx = stream(env.peer().await);
    send(&tx, completed(false, Some(usage(20, 80, 100)))).await;
    tx.send(Ok(Bytes::from_static(
        b"event: error\ndata: {\"error\":{\"message\":\"secret-upstream-credential\"}}\n\n",
    )))
    .await
    .unwrap();
    drop(tx);
    let response = finish(call, 200).await;
    let record = env.record(&response).await;
    assert_usage(&env, &record, 20, 80, 100).await;
    assert_eq!(record["pricing_snapshot"]["media_units"], 1);
    assert_eq!(record["pricing_snapshot"]["image_stream_incomplete"], true);
    let text = response.text().await.unwrap();
    assert!(text.contains("event: image_generation.completed"));
    assert!(text.contains("event: error"));
    assert!(!text.contains("secret-upstream-credential"));
    assert!(!text.contains("[DONE]"));
    env.assert_money(3740, 1).await;
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn preview_only_and_missing_required_usage_refund_without_replay() {
    let mut env = token_env().await;
    for finish_frame in [
        None,
        Some(completed(false, None)),
        Some(json!({"type":"image_generation.completed","b64_json":""})),
    ] {
        let call = launch(env.request(false).json(&body(&env, 1)));
        let tx = stream(env.peer().await);
        send(&tx, preview(false)).await;
        if let Some(frame) = finish_frame {
            send(&tx, frame).await;
        }
        drop(tx);
        let text = finish(call, 200).await.text().await.unwrap();
        assert!(text.contains("event: error"));
        assert!(!text.contains("event: image_generation.completed"));
        env.assert_money(0, 0).await;
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn edits_stream_in_json_and_multipart_with_azure_deployment_auth() {
    let mut env = setup().await;
    env.provider("azure").await;
    let mut json = body(&env, 1);
    json["images"] = json!([{"image_url":"data:image/png;base64,aW1hZ2U="}]);
    for (index, request) in [
        env.request(true).json(&json),
        env.request(true).multipart(
            env.form()
                .text("stream", " true ")
                .text("partial_images", " 01 "),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let call = launch(request);
        let peer = env.peer().await;
        assert_eq!(peer.path, "/openai/deployments/mapped-image/images/edits");
        assert_eq!(peer.headers["api-key"], "image-credential");
        assert!(!peer.headers.contains_key("authorization"));
        if index == 1 {
            assert_eq!(
                peer.uploads
                    .iter()
                    .find(|part| part.name == "partial_images")
                    .unwrap()
                    .bytes,
                "1"
            );
            assert_eq!(
                peer.uploads
                    .iter()
                    .find(|part| part.name == "stream")
                    .unwrap()
                    .bytes,
                "true"
            );
            assert_eq!(
                peer.uploads
                    .iter()
                    .find(|part| part.name == "model")
                    .unwrap()
                    .bytes,
                "mapped-image"
            );
        }
        let tx = stream(peer);
        send(&tx, preview(true)).await;
        send(&tx, completed(true, Some(usage(20, 80, 100)))).await;
        drop(tx);
        let response = finish(call, 200).await;
        let record = env.record(&response).await;
        assert_usage(&env, &record, 20, 80, 100).await;
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("event: image_edit.completed")
        );
    }
    env.assert_money(PRICE * 2, 2).await;
}

#[tokio::test]
async fn json_fallback_returns_completed_events_and_bills_response_usage_once() {
    let mut env = token_env().await;
    let call = launch(env.request(false).json(&body(&env, 3)));
    env.peer().await.raw(
        200,
        token_billing::response(usage(20, 80, 200), 2).to_string(),
    );
    let response = finish(call, 200).await;
    let record = env.record(&response).await;
    assert_usage(&env, &record, 20, 80, 200).await;
    assert_eq!(record["pricing_snapshot"]["image_stream_usage"], "response");
    let text = response.text().await.unwrap();
    assert_eq!(text.matches("event: image_generation.completed").count(), 2);
    assert_eq!(text.matches("input_tokens_details").count(), 1);
    assert!(text.contains("[DONE]"));
    env.assert_money(6740, 1).await;
}

#[tokio::test]
async fn invalid_partial_options_and_async_streaming_fail_before_dispatch() {
    let env = setup().await;
    for value in [json!(-1), json!(4), json!("1"), json!(1.5)] {
        let mut body = body(&env, 1);
        body["partial_images"] = value;
        assert_eq!(
            env.request(false)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    for value in ["-1", "4", "1.5", "", "+1"] {
        assert_eq!(
            env.request(true)
                .multipart(
                    env.form()
                        .text("stream", "true")
                        .text("partial_images", value)
                )
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(
        env.request(true)
            .multipart(env.form().text("stream", "true").text("stream", "false"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let duplicate = format!(
        "{{\"model\":\"{}\",\"prompt\":\"test\",\"stream\":true,\"partial_images\":1,\"partial_images\":2}}",
        env.model
    );
    assert_eq!(
        env.request(false)
            .header("content-type", "application/json")
            .body(duplicate)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    env.state
        .settings_cache
        .insert("image_tasks_enabled".into(), Arc::new(Some(json!(true))))
        .await;
    let reply = reqwest::Client::new()
        .post(format!(
            "http://{}/v1/images/generations/async",
            env.address
        ))
        .bearer_auth(&env.token)
        .json(&body(&env, 1))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 400);
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn final_frames_wait_for_durable_database_settlement() {
    let mut env = setup().await;
    let call = launch(env.request(false).json(&body(&env, 1)));
    let tx = stream(env.peer().await);
    send(&tx, preview(false)).await;
    let mut response = finish(call, 200).await;
    assert!(chunk(&mut response).await.contains("partial_image"));
    let mut lock = env.state.pg.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(env.user)
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    send(&tx, completed(false, None)).await;
    drop(tx);
    assert!(
        timeout(Duration::from_millis(150), response.chunk())
            .await
            .is_err()
    );
    lock.rollback().await.unwrap();
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("event: image_generation.completed")
    );
    env.assert_money(PRICE, 1).await;
}
