use super::*;

#[tokio::test]
async fn two_gateway_instances_share_idempotency_and_the_last_daily_slot() {
    for same in [true, false] {
        let env = Env::new().await;
        sqlx::query("UPDATE api_keys SET rpd_limit=1 WHERE id=$1")
            .bind(env.kid)
            .execute(&env.state.pg)
            .await
            .unwrap();
        let other = gateway::build_state(
            &env.database,
            &std::env::var("OKAPI_REDIS_URL").unwrap(),
            "second-batch-gateway",
            None,
            None,
        )
        .await
        .unwrap();
        other
            .settings_cache
            .insert("image_batches_enabled".into(), Arc::new(Some(json!(true))))
            .await;
        let address = serve(gateway::router(other.clone())).await;
        let first = env
            .request(reqwest::Method::POST, "/v1/images/batches")
            .header("idempotency-key", "first")
            .json(&env.body(1));
        let second = env
            .client
            .post(format!("{address}/v1/images/batches"))
            .bearer_auth(&env.token)
            .header("idempotency-key", if same { "first" } else { "second" })
            .json(&env.body(1));
        let (first, second) = tokio::join!(first.send(), second.send());
        let mut accepted = Vec::new();
        let mut denied = 0;
        for response in [first.unwrap(), second.unwrap()] {
            if response.status() == 202 {
                accepted.push(response.json::<Value>().await.unwrap()["id"].clone());
            } else {
                rejected(response, "rpd").await;
                denied += 1;
            }
        }
        if same {
            assert_eq!(denied, 0);
            assert_eq!(accepted.len(), 2);
            assert_eq!(accepted[0], accepted[1]);
        } else {
            assert_eq!((accepted.len(), denied), (1, 1));
        }
        rows(&env, 1).await;
        let (rpm, _, rpd) = env.state.sched.key_rate_snapshot(env.uid, env.kid).await;
        assert_eq!((rpm, rpd), (1, 1));
        assert!(env.peer.lock().unwrap().calls.is_empty());
        assert_eq!(
            env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
            BALANCE
        );
        other.pg.close().await;
        env.close().await;
    }
}

#[tokio::test]
async fn database_budget_parent_capacity_and_replay_checks_precede_rate_consumption() {
    let env = Env::new().await;
    sqlx::query("UPDATE api_keys SET quota_mode=1,quota_micro=0,max_concurrency=1 WHERE id=$1")
        .bind(env.kid)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let response = post(&env, &env.body(1)).await;
    assert_eq!(response.status(), 429);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["param"],
        "batch_budget"
    );
    assert_eq!(
        env.state.sched.key_rate_snapshot(env.uid, env.kid).await,
        (0, 0, 0)
    );
    sqlx::query("UPDATE api_keys SET quota_mode=0 WHERE id=$1")
        .bind(env.kid)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut body = env.body(1);
    body["parent_batch_id"] = json!(format!("imgbatch_{}", Uuid::new_v4().simple()));
    assert_eq!(post(&env, &body).await.status(), 404);
    assert_eq!(
        env.state.sched.key_rate_snapshot(env.uid, env.kid).await,
        (0, 0, 0)
    );
    let job = env.submit(1, "only").await;
    rejected(post(&env, &env.body(1)).await, "batch_capacity").await;
    let conflict = env
        .request(reqwest::Method::POST, "/v1/images/batches")
        .header("idempotency-key", "only")
        .json(&env.body(2))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), 409);
    assert_eq!(env.submit(1, "only").await["id"], job["id"]);
    rows(&env, 1).await;
    let (rpm, _, rpd) = env.state.sched.key_rate_snapshot(env.uid, env.kid).await;
    assert_eq!((rpm, rpd), (1, 1));
    assert_eq!(
        env.state.ledger.balance(env.uid).await.unwrap().as_micros(),
        BALANCE
    );
    env.close().await;
}

#[tokio::test]
async fn wrong_redis_type_at_last_axis_does_not_partially_write_earlier_axes() {
    use fred::interfaces::ListInterface;
    let env = Env::new().await;
    sqlx::query("UPDATE price_groups SET rph_limit=2 WHERE group_code='default'")
        .execute(&env.state.pg)
        .await
        .unwrap();
    model_limit(&env, 2).await;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let key = format!(
        "rl:{{{}}}:g:default:rph:{}",
        env.uid,
        chrono::Utc::now().timestamp() / 3600
    );
    redis
        .rpush::<i64, _, _>(&key, "not-an-integer-key")
        .await
        .unwrap();
    let response = post(&env, &env.body(1)).await;
    assert_eq!(response.status(), 503);
    assert_eq!(
        env.state.sched.key_rate_snapshot(env.uid, env.kid).await,
        (0, 0, 0)
    );
    let model_key = format!(
        "rl:{{{}}}:m:{}:rpm:{}",
        env.uid,
        env.model,
        chrono::Utc::now().timestamp() / 60
    );
    assert_eq!(redis.get::<Option<i64>, _>(&model_key).await.unwrap(), None);
    rows(&env, 0).await;
    redis.del::<i64, _>(&key).await.unwrap();
    env.submit(2, "after-repair").await;
    assert_eq!(redis.get::<i64, _>(&key).await.unwrap(), 2);
    assert_eq!(redis.get::<i64, _>(&model_key).await.unwrap(), 2);
    env.close().await;
}
