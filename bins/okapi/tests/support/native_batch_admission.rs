use super::*;
use fred::interfaces::KeysInterface;
#[path = "native_batch_admission_edges.rs"]
mod edges;

async fn chat(env: &Env) -> reqwest::Response {
    env.request(reqwest::Method::POST, "/v1/chat/completions")
        .json(&json!({"model":env.model,"messages":[{"role":"user","content":"hello"}],"max_tokens":8}))
        .send().await.unwrap()
}

async fn post(env: &Env, body: &Value) -> reqwest::Response {
    env.request(reqwest::Method::POST, "/v1/images/batches")
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn rejected(response: reqwest::Response, param: &str) {
    assert_eq!(response.status(), 429);
    assert!(response.headers().contains_key("x-okapi-request-id"));
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited", "{body}");
    assert_eq!(body["error"]["param"], param, "{body}");
}

async fn rows(env: &Env, count: i64) {
    let actual: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM image_batches),(SELECT COUNT(*) FROM balance_holds)",
    )
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    assert_eq!(actual, (count, count));
}

async fn model_limit(env: &Env, limit: i64) {
    env.state
        .settings_cache
        .insert(
            "model_rpm_limits".into(),
            Arc::new(Some(json!({env.model.as_str():limit}))),
        )
        .await;
}

#[tokio::test]
async fn native_and_regular_requests_share_daily_limits_in_both_directions() {
    for batch_first in [false, true] {
        let env = Env::new().await;
        sqlx::query("UPDATE api_keys SET rpd_limit=1 WHERE id=$1")
            .bind(env.kid)
            .execute(&env.state.pg)
            .await
            .unwrap();
        if batch_first {
            let job = env.submit(1, "daily").await;
            rejected(chat(&env).await, "rpd").await;
            assert_eq!(env.submit(1, "daily").await["id"], job["id"]);
            assert!(env.peer.lock().unwrap().calls.is_empty());
            rows(&env, 1).await;
            assert_eq!(
                env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
                BALANCE
            );
        } else {
            let response = chat(&env).await;
            assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
            rejected(post(&env, &env.body(1)).await, "rpd").await;
            assert_eq!(env.peer.lock().unwrap().calls.len(), 1);
            rows(&env, 0).await;
            assert_eq!(
                env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
                BALANCE - PRICE
            );
        }
        assert_eq!(
            env.state.sched.key_rate_snapshot(env.uid, env.kid).await.2,
            1
        );
        env.close().await;
    }
}

#[tokio::test]
async fn batch_limits_count_each_upstream_output_before_creating_any_hold() {
    let env = Env::new().await;
    sqlx::query("UPDATE api_keys SET rpm_limit=2,rpd_limit=2 WHERE id=$1")
        .bind(env.kid)
        .execute(&env.state.pg)
        .await
        .unwrap();
    rejected(post(&env, &env.body(3)).await, "rpm").await;
    rows(&env, 0).await;
    assert_eq!(
        env.state.sched.key_rate_snapshot(env.uid, env.kid).await,
        (0, 0, 0)
    );
    let job = env.submit(2, "weighted").await;
    assert_eq!(env.submit(2, "weighted").await["id"], job["id"]);
    rejected(post(&env, &env.body(1)).await, "rpm").await;
    let (rpm, _, rpd) = env.state.sched.key_rate_snapshot(env.uid, env.kid).await;
    assert_eq!((rpm, rpd), (2, 2));
    rows(&env, 1).await;
    assert!(env.peer.lock().unwrap().calls.is_empty());
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        BALANCE
    );
    env.close().await;
}

#[tokio::test]
async fn model_limit_is_shared_by_keys_and_regular_requests_and_rejection_is_atomic() {
    let mut env = Env::new().await;
    model_limit(&env, 2).await;
    rejected(post(&env, &env.body(3)).await, "model_rpm").await;
    assert_eq!(
        env.state.sched.key_rate_snapshot(env.uid, env.kid).await,
        (0, 0, 0)
    );
    assert_eq!(chat(&env).await.status(), 200);
    env.submit(1, "model-last").await;
    let other = format!("sk-other-{}", Uuid::new_v4());
    let kid = okapi_store::provision::create_api_key(
        &env.state.pg,
        env.uid,
        &hex::encode(Sha256::digest(other.as_bytes())),
        "other",
    )
    .await
    .unwrap();
    env.token = other;
    rejected(post(&env, &env.body(1)).await, "model_rpm").await;
    assert_eq!(
        env.state.sched.key_rate_snapshot(env.uid, kid).await,
        (0, 0, 0)
    );
    rows(&env, 1).await;
    env.close().await;
}

#[tokio::test]
async fn group_limits_weight_outputs_and_do_not_consume_other_axes_on_rejection() {
    for (rpm, rph, reason) in [(Some(2), None, "group_rpm"), (None, Some(2), "group_rph")] {
        let env = Env::new().await;
        sqlx::query("UPDATE price_groups SET rpm_limit=$1,rph_limit=$2 WHERE group_code='default'")
            .bind(rpm)
            .bind(rph)
            .execute(&env.state.pg)
            .await
            .unwrap();
        rejected(post(&env, &env.body(3)).await, reason).await;
        assert_eq!(
            env.state.sched.key_rate_snapshot(env.uid, env.kid).await,
            (0, 0, 0)
        );
        env.submit(2, "group-last").await;
        rejected(post(&env, &env.body(1)).await, reason).await;
        rows(&env, 1).await;
        assert_eq!(
            env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
            BALANCE
        );
        env.close().await;
    }
}

#[tokio::test]
async fn concurrent_idempotent_submissions_consume_one_rate_admission() {
    let env = Env::new().await;
    sqlx::query("UPDATE api_keys SET rpm_limit=1,rpd_limit=1 WHERE id=$1")
        .bind(env.kid)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let request = env
            .request(reqwest::Method::POST, "/v1/images/batches")
            .header("idempotency-key", "concurrent")
            .json(&env.body(1));
        calls.spawn(async move { request.send().await.unwrap() });
    }
    let mut ids = std::collections::HashSet::new();
    while let Some(response) = calls.join_next().await {
        let response = response.unwrap();
        assert_eq!(response.status(), 202, "{}", response.text().await.unwrap());
        ids.insert(
            response.json::<Value>().await.unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(ids.len(), 1);
    rows(&env, 1).await;
    let (rpm, _, rpd) = env.state.sched.key_rate_snapshot(env.uid, env.kid).await;
    assert_eq!((rpm, rpd), (1, 1));
    env.close().await;
}

#[tokio::test]
async fn corrupted_rate_storage_fails_closed_without_partial_admission() {
    let env = Env::new().await;
    model_limit(&env, 2).await;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let minute = chrono::Utc::now().timestamp() / 60;
    let model_key = format!("rl:{{{}}}:m:{}:rpm:{minute}", env.uid, env.model);
    for corrupt in ["invalid", "1e2", "1.0", "01", "-1", "9007199254740992"] {
        redis
            .set::<(), _, _>(&model_key, corrupt, None, None, false)
            .await
            .unwrap();
        let response = post(&env, &env.body(1)).await;
        assert_eq!(response.status(), 503, "{corrupt}");
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "overloaded"
        );
        assert_eq!(
            env.state.sched.key_rate_snapshot(env.uid, env.kid).await,
            (0, 0, 0)
        );
        rows(&env, 0).await;
        assert!(env.peer.lock().unwrap().calls.is_empty());
    }
    redis.del::<i64, _>(&model_key).await.unwrap();
    env.submit(1, "recovered").await;
    env.close().await;
}
