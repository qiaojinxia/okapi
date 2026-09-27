use super::*;
use base64::Engine as _;
use okapi::gateway::images::tasks::run_one;
use okapi_store::image_tasks as store;

#[path = "image_tasks_edges.rs"]
mod edges;
#[path = "image_storage_cases.rs"]
mod storage_cases;

async fn enabled_env() -> Env {
    let env = setup().await;
    env.state
        .settings_cache
        .insert("image_tasks_enabled".into(), Arc::new(Some(json!(true))))
        .await;
    env
}
fn url(env: &Env, path: &str) -> String {
    format!("http://{}{path}", env.address)
}
fn id(task: &Value) -> Uuid {
    Uuid::parse_str(
        task["id"]
            .as_str()
            .unwrap()
            .strip_prefix("imgtask_")
            .unwrap(),
    )
    .unwrap()
}
async fn submit(env: &Env, body: Value, key: &str) -> Value {
    let response = reqwest::Client::new()
        .post(url(env, "/v1/images/generations/async"))
        .bearer_auth(&env.token)
        .header("idempotency-key", key)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202, "{}", response.text().await.unwrap());
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.json().await.unwrap()
}
async fn poll(env: &Env, task: &Value) -> Value {
    let response = reqwest::Client::new()
        .get(url(env, task["poll_url"].as_str().unwrap()))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    response.json().await.unwrap()
}
async fn cancel(env: &Env, task: &Value) -> Value {
    let response = reqwest::Client::new()
        .post(url(
            env,
            &format!("{}/cancel", task["poll_url"].as_str().unwrap()),
        ))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    response.json().await.unwrap()
}
fn worker(env: &Env) -> tokio::task::JoinHandle<Result<bool, gateway::error::AppError>> {
    let state = env.state.clone();
    tokio::spawn(async move { run_one(&state).await })
}
async fn join(worker: tokio::task::JoinHandle<Result<bool, gateway::error::AppError>>) {
    assert!(timeout(WAIT, worker).await.unwrap().unwrap().unwrap());
}
async fn expire(env: &Env, task: &Value) {
    sqlx::query("UPDATE image_tasks SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(id(task))
        .execute(&env.state.pg)
        .await
        .unwrap();
}
async fn reserved(env: &Env, request_id: Uuid) {
    let outcome = env
        .state
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: env.user,
                api_key_id: env.key,
                request_id,
                est: Money::from_micros(PRICE),
                caps: okapi_ledger::LimitCaps::default(),
                est_tokens: 0,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        okapi_ledger::ReserveOutcome::Reserved { .. }
    ));
}

#[tokio::test]
async fn queued_request_survives_a_new_worker_state_and_retries_are_idempotent() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(3), "same-request").await;
    assert_eq!(task["status"], "queued");
    assert_eq!(
        submit(&env, env.body(3), "same-request").await["id"],
        task["id"]
    );
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
    let saved: Vec<u8> = sqlx::query_scalar("SELECT payload FROM image_tasks WHERE id=$1")
        .bind(id(&task))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert!(!String::from_utf8(saved).unwrap().contains(&env.token));
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let restarted = gateway::build_state(&database, &redis, "restarted-image-worker", None, None)
        .await
        .unwrap();
    let work = tokio::spawn(async move { run_one(&restarted).await });
    let peer = env.peer().await;
    assert_eq!(peer.body["n"], 3);
    assert_eq!(peer.body["model"], "mapped-image");
    peer.images(1);
    join(work).await;
    let done = poll(&env, &task).await;
    assert_eq!(done["status"], "completed");
    assert_eq!(done["result"]["data"].as_array().unwrap().len(), 1);
    let request_id = Uuid::parse_str(done["request_id"].as_str().unwrap()).unwrap();
    assert_ne!(request_id, id(&task));
    let amount: i64 =
        sqlx::query_scalar("SELECT amount_micro FROM billing_records WHERE request_id=$1")
            .bind(request_id)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(amount, PRICE);
    assert_eq!(
        submit(&env, env.body(3), "same-request").await["status"],
        "completed"
    );
    assert!(!run_one(&env.state).await.unwrap());
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn idempotency_conflicts_and_other_keys_cannot_read_or_cancel_tasks() {
    let env = enabled_env().await;
    let task = submit(&env, env.body(1), "conflict").await;
    let response = reqwest::Client::new()
        .post(url(&env, "/v1/images/generations/async"))
        .bearer_auth(&env.token)
        .header("idempotency-key", "conflict")
        .json(&env.body(2))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let other = format!("sk-other-{}", Uuid::new_v4());
    let hash = hex::encode(Sha256::digest(other.as_bytes()));
    okapi_store::provision::create_api_key(&env.state.pg, env.user, &hash, "other")
        .await
        .unwrap();
    for method in [reqwest::Method::GET, reqwest::Method::POST] {
        let path = if method == reqwest::Method::GET {
            task["poll_url"].as_str().unwrap().to_owned()
        } else {
            format!("{}/cancel", task["poll_url"].as_str().unwrap())
        };
        let response = reqwest::Client::new()
            .request(method, url(&env, &path))
            .bearer_auth(&other)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }
    assert_eq!(cancel(&env, &task).await["status"], "cancelled");
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn multipart_and_base64_results_persist_with_private_downloads() {
    let mut env = enabled_env().await;
    let response = reqwest::Client::new()
        .post(url(&env, "/v1/images/edits/async"))
        .bearer_auth(&env.token)
        .multipart(env.form().text("n", "2"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let task: Value = response.json().await.unwrap();
    let work = worker(&env);
    let peer = env.peer().await;
    assert_eq!(peer.path, "/v1/images/edits");
    assert_eq!(
        peer.uploads
            .iter()
            .find(|p| p.name == "image")
            .unwrap()
            .bytes
            .as_ref(),
        b"\x89PNG\r\n\x00\xff\x01"
    );
    let content = b"\x89PNG\r\n\x1a\nfixture";
    peer.raw(
        200,
        json!({"data":[{"b64_json":base64::prelude::BASE64_STANDARD.encode(content)}]}).to_string(),
    );
    join(work).await;
    let done = poll(&env, &task).await;
    let image = &done["result"]["data"][0];
    assert!(image.get("b64_json").is_none());
    let path = image["url"].as_str().unwrap();
    env.state
        .settings_cache
        .insert("image_tasks_enabled".into(), Arc::new(Some(json!(false))))
        .await;
    assert_eq!(poll(&env, &task).await["status"], "completed");
    let bytes = reqwest::Client::new()
        .get(url(&env, path))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(bytes.status(), 200);
    assert_eq!(bytes.headers()["content-type"], "image/png");
    assert_eq!(bytes.bytes().await.unwrap().as_ref(), content);
    assert_eq!(reqwest::get(url(&env, path)).await.unwrap().status(), 401);
    let cleared:bool=sqlx::query_scalar("SELECT payload IS NULL AND client_ip IS NULL AND NOT billing_pending FROM image_tasks WHERE id=$1")
        .bind(id(&task)).fetch_one(&env.state.pg).await.unwrap();
    assert!(cleared);
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn json_edits_survive_queue_serialization() {
    let mut env = enabled_env().await;
    let mut body = env.body(1);
    body["images"] = json!([{"image_url":"https://image.example/input.png"}]);
    let response = reqwest::Client::new()
        .post(url(&env, "/v1/images/edits/async"))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let task: Value = response.json().await.unwrap();
    let work = worker(&env);
    let peer = env.peer().await;
    body["model"] = json!("mapped-image");
    assert_eq!(peer.body, body);
    peer.images(1);
    join(work).await;
    assert_eq!(poll(&env, &task).await["status"], "completed");
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn concurrent_workers_only_dispatch_once() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(1), "workers").await;
    let work: Vec<_> = (0..4).map(|_| worker(&env)).collect();
    env.peer().await.images(1);
    for work in work {
        timeout(WAIT, work).await.unwrap().unwrap().unwrap();
    }
    assert_eq!(poll(&env, &task).await["status"], "completed");
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn queued_cancellation_is_free_and_in_flight_cancellation_does_not_erase_a_result() {
    let mut env = enabled_env().await;
    let queued = submit(&env, env.body(1), "queued-cancel").await;
    assert_eq!(cancel(&env, &queued).await["status"], "cancelled");
    assert!(!run_one(&env.state).await.unwrap());
    env.assert_money(0, 0).await;
    let running = submit(&env, env.body(1), "running-cancel").await;
    let work = worker(&env);
    let peer = env.peer().await;
    let cancellation = cancel(&env, &running).await;
    assert_eq!(cancellation["status"], "processing");
    assert_eq!(cancellation["cancel_requested"], true);
    peer.images(1);
    join(work).await;
    let done = poll(&env, &running).await;
    assert_eq!(done["status"], "completed");
    assert_eq!(done["cancel_requested"], true);
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn disabled_key_is_rechecked_when_the_worker_claims_a_task() {
    let env = enabled_env().await;
    let task = submit(&env, env.body(1), "recheck").await;
    sqlx::query("UPDATE api_keys SET status=2 WHERE id=$1")
        .bind(env.key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert!(run_one(&env.state).await.unwrap());
    let saved = store::get_owned(&env.state.pg, id(&task), env.user, env.key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, "failed");
    assert_eq!(saved.error.unwrap()["code"], "key_disabled");
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn preparing_recovery_refunds_and_uses_a_new_reservation() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(1), "preparing").await;
    let claimed = store::claim(&env.state.pg).await.unwrap().unwrap();
    assert_eq!(claimed.task.id, id(&task));
    let old = claimed.task.reservation_id.unwrap();
    reserved(&env, old).await;
    expire(&env, &task).await;
    assert!(run_one(&env.state).await.unwrap());
    assert_eq!(poll(&env, &task).await["status"], "queued");
    assert!(
        !store::dispatch(&env.state.pg, id(&task), old, 1, 1)
            .await
            .unwrap()
    );
    env.assert_money(0, 0).await;
    let work = worker(&env);
    let peer = env.peer().await;
    let current = store::get_owned(&env.state.pg, id(&task), env.user, env.key)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(current.reservation_id, Some(old));
    let mut tx = env.state.pg.begin().await.unwrap();
    assert!(!store::lock_live(&mut tx, id(&task), old).await.unwrap());
    tx.rollback().await.unwrap();
    assert!(
        !store::fail(
            &env.state.pg,
            id(&task),
            old,
            502,
            &json!({"code":"old-worker"}),
            false
        )
        .await
        .unwrap()
    );
    peer.images(1);
    join(work).await;
    env.assert_money(PRICE, 1).await;
}

#[tokio::test]
async fn dispatched_crash_is_failed_and_refunded_without_replaying() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(1), "interrupted").await;
    let work = worker(&env);
    let peer = env.peer().await;
    work.abort();
    assert!(work.await.unwrap_err().is_cancelled());
    drop(peer);
    expire(&env, &task).await;
    assert!(run_one(&env.state).await.unwrap());
    assert!(run_one(&env.state).await.unwrap());
    let saved = poll(&env, &task).await;
    assert_eq!(saved["status"], "failed");
    assert_eq!(saved["error"]["param"], "image_task_result_unknown");
    assert_eq!(env.hits.load(Ordering::SeqCst), 1);
    env.assert_money(0, 0).await;
}

#[tokio::test]
async fn failure_refunds_and_pending_redis_settlement_can_be_replayed() {
    let mut env = enabled_env().await;
    let task = submit(&env, env.body(1), "provider-failure").await;
    let work = worker(&env);
    env.peer().await.raw(500, "{}".into());
    join(work).await;
    assert_eq!(poll(&env, &task).await["status"], "failed");
    env.assert_money(0, 0).await;
    let task = submit(&env, env.body(1), "successful").await;
    let work = worker(&env);
    env.peer().await.images(1);
    join(work).await;
    sqlx::query("UPDATE image_tasks SET billing_pending=true WHERE id=$1")
        .bind(id(&task))
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert!(run_one(&env.state).await.unwrap());
    env.assert_money(PRICE, 1).await;
    assert_eq!(env.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn capacity_is_released_by_cancelling_queued_work() {
    let env = enabled_env().await;
    let mut queued = Vec::new();
    for index in 0..3 {
        queued.push(submit(&env, env.body(1), &format!("capacity-{index}")).await);
    }
    let response = reqwest::Client::new()
        .post(url(&env, "/v1/images/generations/async"))
        .bearer_auth(&env.token)
        .json(&env.body(1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    cancel(&env, &queued.remove(0)).await;
    queued.push(submit(&env, env.body(1), "capacity-after-cancel").await);
    for task in queued {
        cancel(&env, &task).await;
    }
    assert_eq!(env.hits.load(Ordering::SeqCst), 0);
    env.assert_money(0, 0).await;
}
