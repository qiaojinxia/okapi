use super::*;
#[path = "native_batch_statistics_time.rs"]
mod time;

async fn member(env: &Env) -> i64 {
    let member =
        okapi_store::provision::create_user(&env.state.pg, &format!("member-{}", Uuid::new_v4()))
            .await
            .unwrap();
    sqlx::query("INSERT INTO team_members(team_user_id,member_user_id,monthly_spend_limit_micro) VALUES($1,$2,$3)")
        .bind(env.uid).bind(member).bind(PRICE / 2).execute(&env.state.pg).await.unwrap();
    sqlx::query("UPDATE api_keys SET member_user_id=$2 WHERE id=$1")
        .bind(env.kid)
        .bind(member)
        .execute(&env.state.pg)
        .await
        .unwrap();
    member
}

#[tokio::test]
async fn native_batch_member_spend_blocks_the_next_paid_request() {
    let env = Env::new().await;
    let member = member(&env).await;
    let job = env.submit(1, "member-spend").await;
    archive::finish(&env, &job).await;
    env.money(&job, PRICE / 2).await;
    assert_eq!(
        env.state.sched.member_spend_get(env.uid, member).await,
        PRICE / 2
    );
    let response = env
        .request(reqwest::Method::POST, "/v1/images/batches")
        .json(&env.body(1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "member_limit_exceeded"
    );
    env.close().await;
}

#[tokio::test]
async fn native_batch_billing_records_elapsed_time_instead_of_zero() {
    let env = Env::new().await;
    let job = env.submit(1, "elapsed").await;
    sqlx::query("UPDATE image_batches SET created_at=now()-interval '2 minutes' WHERE id=$1")
        .bind(id(&job))
        .execute(&env.state.pg)
        .await
        .unwrap();
    archive::finish(&env, &job).await;
    let elapsed: i32 =
        sqlx::query_scalar("SELECT latency_ms FROM billing_records WHERE request_id=$1")
            .bind(id(&job))
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert!(elapsed >= 120_000, "{elapsed}");
    env.close().await;
}

async fn redis() -> fred::clients::Client {
    okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap()
}
async fn delivery(env: &Env, job: &Value) -> okapi_store::image_batches::statistics::Delivery {
    sqlx::query_as("SELECT batch_id,user_id,member_user_id,channel_key_id,recorded_at,tokens,amount_micro,is_error,COALESCE(lease_id,$2) AS lease_id FROM image_batch_statistics WHERE batch_id=$1")
        .bind(id(job)).bind(Uuid::new_v4()).fetch_one(&env.state.pg).await.unwrap()
}
async fn due(env: &Env, job: &Value) {
    sqlx::query("UPDATE image_batch_statistics SET next_attempt_at=now(),lease_until=CASE WHEN lease_id IS NOT NULL THEN now()-interval '1 second' END WHERE batch_id=$1")
        .bind(id(job)).execute(&env.state.pg).await.unwrap();
}

#[tokio::test]
async fn statistics_failure_replays_after_restart_without_rebilling_or_changing_member() {
    use fred::interfaces::KeysInterface;
    let env = Env::new().await;
    let original = member(&env).await;
    let job = env.submit(1, "statistics-recovery").await;
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    let replacement = member(&env).await;
    let redis = redis().await;
    let token_key = format!("tok:{{{}}}:{}", env.uid, chrono::Utc::now().format("%Y%m"));
    redis
        .set::<(), _, _>(&token_key, "not-an-integer", None, None, false)
        .await
        .unwrap();
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    assert_eq!(env.poll(&job).await["status"], "completed");
    assert_eq!(
        env.state.sched.member_spend_get(env.uid, original).await,
        PRICE / 2
    );
    assert_eq!(
        env.state.sched.member_spend_get(env.uid, replacement).await,
        0
    );
    let d = delivery(&env, &job).await;
    assert_eq!(d.member_user_id, Some(original));
    let pending: bool = sqlx::query_scalar(
        "SELECT delivered_at IS NULL FROM image_batch_statistics WHERE batch_id=$1",
    )
    .bind(id(&job))
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    assert!(pending);
    env.value(reqwest::Method::DELETE, &path(&job, "")).await;
    assert!(env.cleanup(&job).await.unwrap());
    assert!(env.cleaned(&job).await);
    let _: i64 = redis.del(&token_key).await.unwrap();
    // Simulate Redis success followed by loss of the PG acknowledgement.
    due(&env, &job).await;
    let claimed = okapi_store::image_batches::statistics::claim(&env.state.pg, Some(id(&job)))
        .await
        .unwrap()
        .unwrap();
    env.state
        .sched
        .record_batch_statistics(&claimed)
        .await
        .unwrap();
    due(&env, &job).await;
    let restarted = gateway::build_state(
        &env.database,
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "statistics-restart",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        gateway::images::batches::run_statistics(&restarted, Some(id(&job)))
            .await
            .unwrap()
    );
    assert!(
        !gateway::images::batches::run_statistics(&restarted, Some(id(&job)))
            .await
            .unwrap()
    );
    assert_eq!(
        restarted.sched.member_spend_get(env.uid, original).await,
        PRICE / 2
    );
    assert_eq!(restarted.sched.monthly_tokens_get(env.uid).await, 22);
    assert_eq!(
        restarted.sched.monthly_spend_get(env.uid).await,
        u64::try_from(PRICE / 2).unwrap()
    );
    assert_eq!(
        restarted
            .sched
            .channel_key_spend_get(d.channel_key_id)
            .await,
        PRICE / 2
    );
    env.money(&job, PRICE / 2).await;
    restarted.pg.close().await;
    env.close().await;
}

#[tokio::test]
async fn statistics_enqueue_failure_keeps_receipt_and_elapsed_stable_for_retry() {
    let env = Env::new().await;
    let job = env.submit(1, "publication-retry").await;
    for _ in 0..3 {
        env.step(&job).await.unwrap();
    }
    sqlx::raw_sql("CREATE FUNCTION reject_batch_stats() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected statistics failure'; END $$; CREATE TRIGGER reject_batch_stats BEFORE INSERT ON image_batch_statistics FOR EACH ROW EXECUTE FUNCTION reject_batch_stats();")
        .execute(&env.state.pg).await.unwrap();
    assert!(env.step(&job).await.is_err());
    assert_eq!(env.poll(&job).await["status"], "settling");
    let receipt: Value = sqlx::query_scalar("SELECT settlement FROM balance_holds WHERE id=$1")
        .bind(id(&job))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert!(receipt["latency_ms"].as_i64().unwrap() > 0);
    sqlx::query("DROP TRIGGER reject_batch_stats ON image_batch_statistics")
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.step(&job).await.unwrap();
    let after: Value = sqlx::query_scalar("SELECT settlement FROM balance_holds WHERE id=$1")
        .bind(id(&job))
        .fetch_one(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(after, receipt);
    assert_eq!(env.poll(&job).await["status"], "completed");
    assert_eq!(env.state.sched.monthly_tokens_get(env.uid).await, 22);
    env.money(&job, PRICE / 2).await;
    env.close().await;
}

#[tokio::test]
async fn queued_batch_rechecks_member_limit_before_provider_submission() {
    let env = Env::new().await;
    let member = member(&env).await;
    let job = env.submit(1, "queued-limit").await;
    env.step(&job).await.unwrap();
    env.state
        .sched
        .member_spend_add(env.uid, member, PRICE / 2)
        .await;
    env.step(&job).await.unwrap();
    env.step(&job).await.unwrap();
    assert_eq!(env.creates(), 0);
    assert_eq!(env.poll(&job).await["status"], "failed");
    env.money(&job, 0).await;
    assert_eq!(
        env.state.sched.member_spend_get(env.uid, member).await,
        PRICE / 2
    );
    env.close().await;
}

#[tokio::test]
async fn batch_statistics_activate_volume_pricing_and_channel_daily_cap() {
    let env = Env::new().await;
    okapi_store::admin::upsert_pricing_rule(&env.state.pg,okapi_store::admin::PricingRuleInput {
        rule_code:"batch-volume",rule_type:"volume",scope:&json!({"users":[env.uid],"models":[env.model]}),
        params:&json!({"multiplier":"0.5","min_monthly_tokens":22,"min_monthly_spend_micro":20000}),
        priority:0,enabled:true,valid_from:None,valid_to:None,
    }).await.unwrap();
    crate::published_pricing::publish(&env.state.pg, env.uid).await;
    gateway::refresh_pricebook_if_newer(&env.state)
        .await
        .unwrap();
    let first = env.submit(1, "before-volume").await;
    archive::finish(&env, &first).await;
    let second = env.submit(1, "after-volume").await;
    archive::finish(&env, &second).await;
    let amount: i64 =
        sqlx::query_scalar("SELECT amount_micro FROM billing_records WHERE request_id=$1")
            .bind(id(&second))
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(amount, PRICE / 4);
    let d = delivery(&env, &second).await;
    sqlx::query("UPDATE channel_keys SET daily_spend_cap_micro=$2 WHERE id=$1")
        .bind(d.channel_key_id)
        .bind(PRICE / 2 + PRICE / 4)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let response = env
        .request(reqwest::Method::POST, "/v1/images/batches")
        .json(&env.body(1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(env.creates(), 2);
    assert_eq!(env.state.sched.monthly_tokens_get(env.uid).await, 44);
    assert_eq!(
        env.state.sched.monthly_spend_get(env.uid).await,
        u64::try_from(PRICE / 2 + PRICE / 4).unwrap()
    );
    env.close().await;
}
