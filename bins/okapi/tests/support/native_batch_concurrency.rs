use super::*;

pub(super) async fn chat_peer(
    peer: &Arc<Mutex<Peer>>,
    parts: &axum::http::request::Parts,
    value: Value,
) -> Response {
    let gate = {
        let mut peer = peer.lock().unwrap();
        peer.calls
            .push((parts.method.clone(), parts.uri.path().into(), value));
        peer.chat_gate.clone()
    };
    if let Some(gate) = gate {
        gate.acquire().await.unwrap().forget();
    }
    axum::Json(json!({"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":2,"totalTokenCount":10}})).into_response()
}

fn chat(env: &Env) -> reqwest::RequestBuilder {
    env.request(reqwest::Method::POST, "/v1/chat/completions")
        .json(&json!({"model":env.model,"messages":[{"role":"user","content":"hello"}],"max_tokens":8}))
}

async fn one_slot(env: &Env) {
    sqlx::query("UPDATE api_keys SET max_concurrency=1 WHERE id=$1")
        .bind(env.kid)
        .execute(&env.state.pg)
        .await
        .unwrap();
}

async fn denied(env: &Env) {
    let response = chat(env).send().await.unwrap();
    assert_eq!(response.status(), 429);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["error"]["code"], "rate_limited");
    assert_eq!(value["error"]["param"], "concurrency");
}

async fn settled_chat(env: &Env, response: reqwest::Response) {
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    env.state
        .settlements
        .wait_idle(Duration::from_secs(5))
        .await;
    assert_eq!(env.state.settlements.in_flight(), 0);
    assert!(
        env.state
            .ledger
            .list_reservations(env.uid)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn funded_batch_occupies_the_key_slot_until_cancellation_is_settled() {
    let env = Env::new().await;
    one_slot(&env).await;
    let job = env.submit(1, "shared-slot").await;
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "preparing");
    denied(&env).await;
    assert!(env.peer.lock().unwrap().calls.is_empty());
    env.value(reqwest::Method::POST, &path(&job, "/cancel"))
        .await;
    denied(&env).await;
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    env.money(&job, 0).await;
    assert_eq!(env.creates(), 0);
    settled_chat(&env, chat(&env).send().await.unwrap()).await;
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        BALANCE - PRICE
    );
    env.close().await;
}

#[tokio::test]
async fn normal_http_inflight_keeps_batch_funding_pending_without_freezing_or_failing() {
    let env = Env::new().await;
    one_slot(&env).await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    env.peer.lock().unwrap().chat_gate = Some(gate.clone());
    let request = chat(&env);
    let ordinary = tokio::spawn(async move { request.send().await.unwrap() });
    for _ in 0..100 {
        if !env.peer.lock().unwrap().calls.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(env.peer.lock().unwrap().calls.len(), 1);
    let before = env.state.ledger.balance(env.uid).await.unwrap();
    let job = env.submit(1, "waiting-slot").await;
    for _ in 0..2 {
        env.step(&job).await.unwrap();
        assert_eq!(env.poll(&job).await["status"], "funding");
        assert!(env.poll(&job).await["error"].is_null());
        let held: String = sqlx::query_scalar("SELECT state FROM balance_holds WHERE id=$1")
            .bind(id(&job))
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
        assert_eq!(held, "pending");
        assert_eq!(env.state.ledger.balance(env.uid).await.unwrap(), before);
    }
    assert_eq!(env.creates(), 0);
    gate.add_permits(1);
    settled_chat(&env, ordinary.await.unwrap()).await;
    env.peer.lock().unwrap().chat_gate = None;
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "preparing");
    denied(&env).await;
    archive::finish(&env, &job).await;
    assert_eq!(env.poll(&job).await["status"], "completed");
    settled_chat(&env, chat(&env).send().await.unwrap()).await;
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        BALANCE - PRICE * 2 - PRICE / 2
    );
    env.close().await;
}

#[tokio::test]
async fn uncertain_submission_keeps_slot_across_worker_restart_until_actual_settlement() {
    let env = Env::new().await;
    one_slot(&env).await;
    env.peer.lock().unwrap().create_status = 502;
    let job = env.submit(1, "uncertain-slot").await;
    env.step(&job).await.unwrap();
    assert!(env.step(&job).await.is_err());
    assert_eq!(env.poll(&job).await["status"], "uncertain");
    denied(&env).await;
    let restarted = gateway::build_state(
        &env.database,
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "restart-concurrency",
        None,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE image_batches SET next_run_at=now() WHERE id=$1")
        .bind(id(&job))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert!(run_one(&restarted, Some(id(&job))).await.unwrap());
    denied(&env).await;
    archive::finish(&env, &job).await;
    env.money(&job, PRICE / 2).await;
    assert_eq!(env.creates(), 1);
    settled_chat(&env, chat(&env).send().await.unwrap()).await;
    restarted.pg.close().await;
    env.close().await;
}

#[tokio::test]
async fn cancellation_while_waiting_never_releases_the_ordinary_requests_slot() {
    let env = Env::new().await;
    one_slot(&env).await;
    let ordinary = Uuid::new_v4();
    let reserve = okapi_ledger::ReserveRequest {
        user_id: env.uid,
        api_key_id: env.kid,
        request_id: ordinary,
        est: Money::from_micros(PRICE),
        est_tokens: 0,
        caps: okapi_ledger::LimitCaps {
            concurrency: 1,
            ..Default::default()
        },
    };
    assert!(matches!(
        env.state
            .ledger
            .reserve(reserve, chrono::Utc::now())
            .await
            .unwrap(),
        okapi_ledger::ReserveOutcome::Reserved { .. }
    ));
    let job = env.submit(1, "cancel-waiting").await;
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "funding");
    env.value(reqwest::Method::POST, &path(&job, "/cancel"))
        .await;
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "cancelled");
    denied(&env).await;
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        BALANCE - PRICE
    );
    assert!(env.peer.lock().unwrap().calls.is_empty());
    env.state
        .ledger
        .refund(env.uid, env.kid, ordinary)
        .await
        .unwrap();
    env.money(&job, 0).await;
    assert_eq!(chat(&env).send().await.unwrap().status(), 200);
    env.close().await;
}
