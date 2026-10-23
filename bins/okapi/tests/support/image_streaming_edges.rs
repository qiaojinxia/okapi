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

/// 候选渠道只是准入被拒（单 key 并发已满）时，请求还没发给上游，要换下一个渠道——非流式图片路径和
/// chat 主链都这样；流式路径此前直接回 502，池里其余健康渠道白白闲着。
#[tokio::test]
async fn admission_rejection_fails_over_to_the_next_candidate() {
    use okapi::gateway::sched_redis::channel_permit::ChannelPermit;
    let mut env = setup().await;
    let first_key: i64 = sqlx::query_scalar("SELECT id FROM channel_keys WHERE channel_id=$1")
        .bind(env.channels[0])
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    // 第一个渠道排在前面，且它唯一的 key 并发上限 1 已被占满
    sqlx::query(
        "UPDATE channels SET priority = CASE WHEN id=$1 THEN 10 ELSE 0 END WHERE id=ANY($2)",
    )
    .bind(env.channels[0])
    .bind(&env.channels)
    .execute(&env.state.pg)
    .await
    .unwrap();
    sqlx::query("UPDATE channel_keys SET max_concurrency=1 WHERE id=$1")
        .bind(first_key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let held = ChannelPermit::acquire_key(&env.state.sched, first_key, Some(1))
        .await
        .unwrap()
        .expect("占满第一个渠道的并发");

    let call = launch(env.request(false).json(&body(&env, 1)));
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
    assert_eq!(env.hits.load(Ordering::SeqCst), 1, "被拒的渠道不该收到请求");
    held.release().await;
}

/// 所有候选都是准入被拒：回 503 `no_available_channel`（与非流式路径同口径），不是 502。
#[tokio::test]
async fn all_candidates_rejected_by_admission_answer_503() {
    use okapi::gateway::sched_redis::channel_permit::ChannelPermit;
    let env = setup().await;
    let keys: Vec<i64> = sqlx::query_scalar("SELECT id FROM channel_keys WHERE channel_id=ANY($1)")
        .bind(&env.channels)
        .fetch_all(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channel_keys SET max_concurrency=1 WHERE id=ANY($1)")
        .bind(&keys)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut held = Vec::new();
    for key in keys {
        held.push(
            ChannelPermit::acquire_key(&env.state.sched, key, Some(1))
                .await
                .unwrap()
                .expect("占满并发"),
        );
    }
    let response = env
        .request(false)
        .json(&body(&env, 1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("no_available_channel")
    );
    env.assert_money(0, 0).await;
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    for permit in held {
        permit.release().await;
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
            "SELECT amount_micro,pool,source_window FROM billing_records WHERE request_id=$1 AND log_type=2",
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
