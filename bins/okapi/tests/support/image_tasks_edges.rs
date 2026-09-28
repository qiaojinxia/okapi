use super::*;
use axum::extract::{Path, State};

fn settlement(env: &Env, request_id: Uuid) -> okapi_ledger::SettlementInput<'_> {
    okapi_ledger::SettlementInput {
        source_window: None,
        dimensions: okapi_ledger::pg::UsageDimensions::new(
            &env.model,
            "mapped-image",
            "/v1/images/generations/async",
            "/v1/images/generations",
        ),
        request_id,
        log_type: 2,
        user_id: env.user,
        api_key_id: env.key,
        group_code: "default",
        model_name: &env.model,
        channel_id: Some(env.channels[0]),
        channel_key_id: None,
        state: okapi_domain::BillingState::Committed,
        usage: okapi_domain::TokenUsage::default(),
        amount: Money::from_micros(PRICE),
        original: Money::from_micros(PRICE),
        discount: Money::ZERO,
        list_price: Money::from_micros(PRICE),
        upstream_cost: None,
        pricing_epoch: None,
        pricing_snapshot: None,
        latency_ms: 10,
        ttft_ms: None,
        is_stream: false,
        retry_count: 0,
        failover_count: 0,
        upstream_status: Some(200),
        error_code: None,
        upstream_request_id: None,
        node: "image-recovery-test",
        sticky_layer: 0,
        client_type: "test",
        client_ip: None,
        delta_micro: -PRICE,
        balance_after: None,
        event_type: "commit",
        pool: okapi_ledger::Pool::Wallet,
    }
}

#[tokio::test]
async fn committed_result_survives_before_redis_settlement_and_cleanup_waits_for_replay() {
    let env = enabled_env().await;
    let task = submit(&env, env.body(3), "commit-crash").await;
    let claimed = store::claim(&env.state.pg).await.unwrap().unwrap();
    assert_eq!(claimed.task.id, id(&task));
    let reservation = claimed.task.reservation_id.unwrap();
    env.state
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: env.user,
                api_key_id: env.key,
                request_id: reservation,
                est: Money::from_micros(PRICE * 3),
                caps: okapi_ledger::LimitCaps::default(),
                est_tokens: 0,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let mut tx = env.state.pg.begin().await.unwrap();
    assert!(
        store::lock_live(&mut tx, id(&task), reservation)
            .await
            .unwrap()
    );
    okapi_ledger::pg::record_settlement_in_tx(&mut tx, settlement(&env, reservation))
        .await
        .unwrap();
    store::complete(
        &mut tx,
        id(&task),
        &json!({"data":[{"url":"/private-image"}]}),
        200,
        &[store::Artifact {
            index: 0,
            content: b"persisted".to_vec(),
            content_type: "application/octet-stream".into(),
        }],
    )
    .await
    .unwrap();
    let visible: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE request_id=$1")
            .bind(reservation)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(visible, 0);
    // The expiry sweeper and result writer must serialize on this same task row.
    let mut balance_lock = Box::pin(store::lock_for_balance(&env.state.pg, reservation));
    assert!(
        timeout(Duration::from_millis(50), &mut balance_lock)
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    drop(timeout(WAIT, balance_lock).await.unwrap().unwrap());
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        BALANCE - PRICE * 3
    );
    assert_eq!(poll(&env, &task).await["status"], "completed");
    // Simulate process loss after PG commit, before Redis commit. Even expiry cannot
    // delete the result/attempt mapping until the persistent settlement marker clears.
    sqlx::query("UPDATE image_tasks SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(id(&task))
        .execute(&env.state.pg)
        .await
        .unwrap();
    store::cleanup(&env.state.pg).await.unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM image_tasks WHERE id=$1)")
        .bind(id(&task))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert!(exists);
    assert!(run_one(&env.state).await.unwrap());
    env.assert_money(PRICE, 1).await;
    store::cleanup(&env.state.pg).await.unwrap();
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT COUNT(*) FROM image_tasks WHERE id=$1),(SELECT COUNT(*) FROM image_task_artifacts WHERE task_id=$1),(SELECT COUNT(*) FROM image_task_attempts WHERE task_id=$1)").bind(id(&task)).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn admission_validates_before_queueing_and_alias_creation_is_idempotent() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let endpoint = url(&env, "/images/generations/async");
    assert_eq!(
        client
            .post(&endpoint)
            .json(&env.body(1))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&env.token)
            .json(&env.body(1))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    env.state
        .settings_cache
        .insert("image_tasks_enabled".into(), Arc::new(Some(json!(true))))
        .await;
    for (body, idem) in [
        (env.body(0), "valid"),
        (env.body(11), "valid"),
        (env.body(1), "contains space"),
    ] {
        assert_eq!(
            client
                .post(&endpoint)
                .bearer_auth(&env.token)
                .header("idempotency-key", idem)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM image_tasks WHERE user_id=$1")
        .bind(env.user)
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let body = env.body(1);
    let create = || {
        client
            .post(&endpoint)
            .bearer_auth(&env.token)
            .header("idempotency-key", "parallel")
            .json(&body)
            .send()
    };
    let (a, b) = tokio::join!(create(), create());
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.status(), 202);
    assert_eq!(b.status(), 202);
    let a: Value = a.json().await.unwrap();
    let b: Value = b.json().await.unwrap();
    assert_eq!(a["id"], b["id"]);
    let alias = a["poll_url"].as_str().unwrap().strip_prefix("/v1").unwrap();
    assert_eq!(
        client
            .get(url(&env, alias))
            .bearer_auth(&env.token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    cancel(&env, &a).await;
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn result_storage_failure_rolls_back_billing_and_refunds_the_reservation() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(1), "rollback").await;
    let work = worker(&env);
    // Valid upstream Images shape, but metadata cannot be persisted within the task budget.
    // The ledger insert precedes this failure inside the shared transaction.
    env.peer().await.raw(200,json!({"data":[{"url":"https://image.example/result.png","revised_prompt":"x".repeat(store::MAX_RESULT_METADATA_BYTES)}]}).to_string());
    join(work).await;
    let saved = poll(&env, &task).await;
    assert_eq!(saved["status"], "failed");
    assert!(saved.get("result").is_none());
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_events WHERE request_id=$1")
        .bind(Uuid::parse_str(saved["request_id"].as_str().unwrap()).unwrap())
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(events, 0);
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn downloads_hold_capacity_until_body_drop_and_do_not_require_a_balance() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(1), "download").await;
    let work = worker(&env);
    env.peer().await.raw(
        200,
        json!({"data":[{"b64_json":base64::prelude::BASE64_STANDARD.encode(b"private image")}]})
            .to_string(),
    );
    join(work).await;
    env.state
        .ledger
        .credit(env.user, Money::from_micros(-(BALANCE - PRICE)))
        .await
        .unwrap();
    assert_eq!(poll(&env, &task).await["status"], "completed");
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", env.token).parse().unwrap(),
    );
    let download = || {
        okapi::gateway::images::tasks::content(
            State(env.state.clone()),
            Path((task["id"].as_str().unwrap().to_owned(), "0".into())),
            headers.clone(),
        )
    };
    let mut held = Vec::new();
    for _ in 0..4 {
        let response = download().await;
        assert_eq!(response.status(), 200);
        held.push(response);
    }
    assert_eq!(download().await.status(), 429);
    held.pop();
    let response = download().await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .as_ref(),
        b"private image"
    );
    drop(held);
    assert_eq!(env.state.image_download_gate.available_permits(), 4);
    let other = format!("sk-download-other-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(
        &env.state.pg,
        env.user,
        &hex::encode(Sha256::digest(other.as_bytes())),
        "other",
    )
    .await
    .unwrap();
    let path = format!("{}/content/0", task["poll_url"].as_str().unwrap());
    assert_eq!(
        reqwest::Client::new()
            .get(url(&env, &path))
            .bearer_auth(other)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn metadata_count_quota_and_expiry_prevent_unbounded_cancelled_tasks() {
    let env = enabled_env().await;
    let task = submit(&env, env.body(1), "expires").await;
    cancel(&env, &task).await;
    let budget: i64 = sqlx::query_scalar("SELECT storage_budget FROM image_tasks WHERE id=$1")
        .bind(id(&task))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(budget, store::TASK_OVERHEAD_BYTES);
    let limits = store::Limits {
        per_user_tasks: 1,
        ..Default::default()
    };
    let admission = store::enqueue(
        &env.state.pg,
        store::NewTask {
            id: Uuid::new_v4(),
            user_id: env.user,
            api_key_id: env.key,
            kind: "generation",
            model: &env.model,
            request_hash: &"0".repeat(64),
            idempotency_hash: None,
            payload: b"{}",
            client_ip: None,
            client_type: "test",
        },
        limits,
    )
    .await
    .unwrap();
    assert!(matches!(admission, store::Enqueued::Capacity));
    sqlx::query("UPDATE image_tasks SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(id(&task))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(
        reqwest::Client::new()
            .get(url(&env, task["poll_url"].as_str().unwrap()))
            .bearer_auth(&env.token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let replacement = submit(&env, env.body(1), "expires").await;
    assert_ne!(replacement["id"], task["id"]);
    cancel(&env, &replacement).await;
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn worker_rechecks_model_and_ip_permissions_after_queueing() {
    let env = enabled_env().await;
    for (name, model, ip, code) in [
        ("model", Some(json!([])), None, "model_not_allowed"),
        ("ip", None, Some(json!(["192.0.2.0/24"])), "ip_not_allowed"),
    ] {
        let task = submit(&env, env.body(1), name).await;
        sqlx::query("UPDATE api_keys SET model_allowlist=$2,ip_allowlist=$3 WHERE id=$1")
            .bind(env.key)
            .bind(model)
            .bind(ip)
            .execute(&env.state.pg)
            .await
            .unwrap();
        assert!(run_one(&env.state).await.unwrap());
        let saved = store::get_owned(&env.state.pg, id(&task), env.user, env.key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.status, "failed");
        assert_eq!(saved.error.unwrap()["code"], code);
        sqlx::query("UPDATE api_keys SET model_allowlist=NULL,ip_allowlist=NULL WHERE id=$1")
            .bind(env.key)
            .execute(&env.state.pg)
            .await
            .unwrap();
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn worker_stop_drains_a_running_generation_and_leaves_new_work_queued() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(1), "drain").await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let work = tokio::spawn(okapi::gateway::images::tasks::run_worker(
        env.state.clone(),
        stopped,
    ));
    let peer = env.peer().await;
    stop.send(true).unwrap();
    peer.images(1);
    timeout(WAIT, work).await.unwrap().unwrap();
    assert_eq!(poll(&env, &task).await["status"], "completed");
    let next = submit(&env, env.body(1), "after-stop").await;
    assert_eq!(next["status"], "queued");
    cancel(&env, &next).await;
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
    env.assert_money(PRICE, 1).await;
}
