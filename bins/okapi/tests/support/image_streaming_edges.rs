use super::*;

#[tokio::test]
async fn rejected_http_requests_only_fail_over_for_explicit_refusals() {
    for status in [401, 402, 403, 429, 408, 500, 503, 307] {
        let mut env = setup().await;
        let call = launch(env.request(false).json(&body(&env, 1)));
        env.peer().await.raw(status, "upstream-secret".into());
        if matches!(status, 401 | 402 | 403 | 429) {
            let tx = stream(env.peer().await);
            send(&tx, completed(false, None)).await;
            drop(tx);
            let response = finish(call, 200).await;
            assert_eq!(env.record(&response).await["failover_count"], 1);
            assert!(
                response
                    .text()
                    .await
                    .unwrap()
                    .contains("image_generation.completed")
            );
            env.assert_money(PRICE, 1).await;
            assert_eq!(env.hits.load(Ordering::SeqCst), 2);
        } else {
            assert!(
                !finish(call, 502)
                    .await
                    .text()
                    .await
                    .unwrap()
                    .contains("upstream-secret")
            );
            env.assert_money(0, 0).await;
            assert_eq!(env.hits.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn corrupt_frame_duplicate_fields_and_event_type_mismatch_refund() {
    let mut env = setup().await;
    let cases = [
        "event: image_generation.completed\ndata: invalid-json\n\n",
        "event: image_edit.completed\ndata: {\"type\":\"image_generation.completed\",\"b64_json\":\"aA==\"}\n\n",
        "data: {\"type\":\"image_generation.completed\",\"type\":\"image_generation.completed\",\"b64_json\":\"aA==\"}\n\n",
        "data: {\"type\":\"image_generation.partial_image\",\"partial_image_index\":3,\"b64_json\":\"aA==\"}\n\n",
        "data: [DONE]\n\n",
    ];
    for raw in cases {
        let call = launch(env.request(false).json(&body(&env, 1)));
        let tx = stream(env.peer().await);
        tx.send(Ok(Bytes::from_static(raw.as_bytes())))
            .await
            .unwrap();
        drop(tx);
        let response = finish(call, 200).await.text().await.unwrap();
        assert!(response.contains("event: error"));
        assert!(!response.contains("event: image_generation.completed"));
        assert!(!response.contains("[DONE]"));
        env.assert_money(0, 0).await;
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), cases.len());
}

#[tokio::test]
async fn split_sse_crlf_comments_and_multiline_json_preserve_completion() {
    let mut env = setup().await;
    let call = launch(env.request(false).json(&body(&env, 1)));
    let tx = stream(env.peer().await);
    let raw = ": ping\r\nevent: image_generation.completed\r\ndata: {\"type\":\"image_generation.completed\",\r\ndata: \"b64_json\":\"aA==\"}\r\n\r\n";
    for bytes in raw.as_bytes().chunks(7) {
        tx.send(Ok(Bytes::copy_from_slice(bytes))).await.unwrap();
    }
    drop(tx);
    let text = finish(call, 200).await.text().await.unwrap();
    assert!(text.contains("event: image_generation.completed"));
    assert!(!text.contains("event: error"));
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn extra_completed_frames_cannot_bill_beyond_requested_count() {
    let mut env = setup().await;
    let call = launch(env.request(false).json(&body(&env, 1)));
    let tx = stream(env.peer().await);
    send(&tx, completed(false, None)).await;
    send(&tx, completed(false, None)).await;
    drop(tx);
    let response = finish(call, 200).await;
    let record = env.record(&response).await;
    assert_eq!(record["pricing_snapshot"]["image_stream_incomplete"], true);
    let text = response.text().await.unwrap();
    assert_eq!(text.matches("event: image_generation.completed").count(), 1);
    assert!(text.contains("event: error"));
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn cumulative_axis_regression_keeps_only_verified_prefix() {
    let mut env = token_env().await;
    let call = launch(env.request(false).json(&body(&env, 2)));
    let tx = stream(env.peer().await);
    send(&tx, completed(false, Some(usage(20, 80, 100)))).await;
    send(&tx, completed(false, Some(usage(10, 90, 200)))).await;
    drop(tx);
    let response = finish(call, 200).await;
    assert_usage(&env, &env.record(&response).await, 20, 80, 100).await;
    assert!(response.text().await.unwrap().contains("event: error"));
    env.assert_money(3740, 1).await;
}

#[tokio::test]
async fn preview_estimate_obeys_tpm_before_dispatch() {
    let env = token_env().await;
    // 10 prompt bytes + 1,000 model output estimate + 100 for the requested preview.
    sqlx::query("UPDATE api_keys SET tpm_limit=1050 WHERE id=$1")
        .bind(env.key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let response = env
        .request(false)
        .json(&body(&env, 1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "rate_limited"
    );
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn invalid_usage_mode_fails_without_dispatch_and_releases_reserve() {
    let env = setup().await;
    sqlx::query("UPDATE channels SET settings=jsonb_set(settings,'{image_stream_usage}','\"guess\"') WHERE id=ANY($1)")
        .bind(&env.channels).execute(&env.state.pg).await.unwrap();
    assert_eq!(
        env.request(false)
            .json(&body(&env, 1))
            .send()
            .await
            .unwrap()
            .status(),
        500
    );
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn unterminated_oversized_frame_is_bounded_before_parsing() {
    let mut env = setup().await;
    let call = launch(env.request(false).json(&body(&env, 1)));
    let tx = stream(env.peer().await);
    tx.send(Ok(Bytes::from_static(b"data: {\"b64_json\":\"")))
        .await
        .unwrap();
    let bytes = Bytes::from(vec![b'a'; 1024 * 1024]);
    for _ in 0..65 {
        if tx.send(Ok(bytes.clone())).await.is_err() {
            break;
        }
    }
    drop(tx);
    let text = finish(call, 200).await.text().await.unwrap();
    assert!(text.contains("event: error"));
    assert!(!text.contains("b64_json"));
    env.assert_money(0, 0).await;
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pricing_and_usage_mode_are_frozen_while_stream_is_open() {
    let mut env = token_env().await;
    let epoch = env.state.pricebook.load().epoch();
    let call = launch(env.request(false).json(&body(&env, 2)));
    let tx = stream(env.peer().await);
    send(&tx, preview(false)).await;
    let mut response = finish(call, 200).await;
    chunk(&mut response).await;
    sqlx::query("UPDATE channels SET settings=jsonb_set(settings,'{image_stream_usage}','\"per_image\"') WHERE id=ANY($1)")
        .bind(&env.channels).execute(&env.state.pg).await.unwrap();
    env.state.pricebook.replace(
        okapi_pricing::book::compile(okapi_pricing::PriceBookSource {
            epoch: epoch + 1,
            models: vec![],
            groups: vec![],
            overrides: vec![],
            rules: vec![],
        })
        .unwrap(),
    );
    send(&tx, completed(false, Some(usage(20, 80, 100)))).await;
    send(&tx, completed(false, Some(usage(20, 80, 200)))).await;
    drop(tx);
    let record = env.record(&response).await;
    assert_eq!(record["pricing_epoch"], epoch);
    assert_eq!(
        record["pricing_snapshot"]["image_stream_usage"],
        "cumulative"
    );
    assert_usage(&env, &record, 20, 80, 200).await;
    env.assert_money(6740, 1).await;
}

async fn subscription(env: &Env, quota: i64) -> okapi_store::subscriptions::Subscription {
    let code = format!("image-sse-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO plans(plan_code,display_name,grant_micro,kind,period,duration_days) VALUES($1,'SSE contract',$2,1,1,30)")
        .bind(&code).bind(quota).execute(&env.state.pg).await.unwrap();
    let plan = okapi_store::subscriptions::find_sub_plan(&env.state.pg, &code)
        .await
        .unwrap()
        .unwrap();
    okapi_ledger::subscriptions::grant(
        &env.state.pg,
        &env.state.ledger,
        env.user,
        &plan,
        &code,
        "test:sse",
    )
    .await
    .unwrap()
    .subscription()
    .clone()
}

#[tokio::test]
async fn success_and_refund_after_replacement_cannot_change_new_subscription() {
    for success in [true, false] {
        let mut env = setup().await;
        sqlx::query("UPDATE users SET balance_micro=0 WHERE id=$1")
            .bind(env.user)
            .execute(&env.state.pg)
            .await
            .unwrap();
        okapi_ledger::pg::record_credit(
            &env.state.pg,
            env.user,
            Money::from_micros(BALANCE),
            "adjust",
            "test",
            json!({}),
        )
        .await
        .unwrap();
        let old = subscription(&env, PRICE * 3).await;
        let call = launch(env.request(false).json(&body(&env, 2)));
        let tx = stream(env.peer().await);
        send(&tx, preview(false)).await;
        let mut response = finish(call, 200).await;
        chunk(&mut response).await;
        let reserves = env.state.ledger.list_reservations(env.user).await.unwrap();
        assert_eq!(reserves.len(), 1);
        assert_eq!(reserves[0].pool, okapi_ledger::Pool::Subscription);
        okapi_ledger::subscriptions::end(&env.state.pg, &env.state.ledger, old.id, 3, "test:sse")
            .await
            .unwrap();
        subscription(&env, PRICE * 5).await;
        if success {
            send(&tx, completed(false, None)).await;
        }
        drop(tx);
        let id = response.headers()["x-okapi-request-id"]
            .to_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap();
        let text = response.text().await.unwrap();
        assert_eq!(text.contains("event: error"), !success);
        env.state.settlements.wait_idle(WAIT).await;
        assert!(
            env.state
                .ledger
                .list_reservations(env.user)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            env.state
                .ledger
                .sub_balance(env.user)
                .await
                .unwrap()
                .0
                .as_micros(),
            PRICE * 5
        );
        assert_eq!(
            env.state
                .ledger
                .balance(env.user)
                .await
                .unwrap()
                .as_micros(),
            BALANCE
        );
        let bill: Option<(i64, i16, Option<String>)> = sqlx::query_as(
            "SELECT amount_micro,pool,source_window FROM billing_records WHERE request_id=$1",
        )
        .bind(id)
        .fetch_optional(&env.state.pg)
        .await
        .unwrap();
        if success {
            assert_eq!(bill, Some((PRICE, 1, reserves[0].source_window.clone())));
        } else {
            assert!(bill.is_none());
        }
    }
}
