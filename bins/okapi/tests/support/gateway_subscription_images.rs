use super::*;

async fn image_case(failed: bool, lose_hot: bool) -> TestResult {
    let database = database().await?;
    let bed = Bed::new_at(false, &database).await?;
    okapi_ledger::pg::record_credit(
        &bed.pg,
        bed.uid,
        Money::from_micros(10_000_000),
        "adjust",
        "test",
        json!({}),
    )
    .await?;
    let sub = grant(&bed, 2000, "image").await?;
    let state = image_state(&bed, &database).await?;
    let address = serve(gateway::router(state.clone())).await?;
    let response = reqwest::Client::new()
        .post(format!("http://{address}/v1/images/generations/async"))
        .bearer_auth(&bed.token)
        .json(&json!({"model":bed.model,"prompt":"window test","n":3}))
        .send()
        .await?;
    assert_eq!(response.status(), 202);
    let task: Value = response.json().await?;
    bed.gate
        .mode
        .store(if failed { 2 } else { 4 }, Ordering::SeqCst);
    let worker = {
        let state = state.clone();
        tokio::spawn(async move { gateway::images::tasks::run_one(&state).await })
    };
    tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
    let reservations = bed.ledger.list_reservations(bed.uid).await?;
    assert_eq!(reservations.len(), 1);
    assert_eq!(reservations[0].amount.as_micros(), 72);
    let expected = transition(
        &bed,
        &sub,
        if failed {
            Change::Cancel
        } else {
            Change::Replace
        },
    )
    .await?;
    if lose_hot {
        bed.redis
            .del::<(), _>(format!("bal:{{{}}}", bed.uid))
            .await?;
    }
    bed.gate.release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await??
            .expect("image worker")
    );
    let response = reqwest::Client::new()
        .get(format!(
            "http://{address}{}",
            task["poll_url"].as_str().ok_or("poll URL")?
        ))
        .bearer_auth(&bed.token)
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let done: Value = response.json().await?;
    assert_eq!(done["status"], if failed { "failed" } else { "completed" });
    if !failed {
        assert_image_bill_and_content(&bed, address, &done, &reservations[0]).await?;
    }
    let pending: (i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM billing_sync WHERE user_id=$1),(SELECT COUNT(*) FROM image_tasks WHERE user_id=$1 AND billing_pending)")
        .bind(bed.uid).fetch_one(&bed.pg).await?;
    assert_eq!(pending, (0, 0));
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await?.0.as_micros(),
        expected
    );
    assert_eq!(repair(&bed).await?.subscription, expected);
    assert_eq!(bed.ledger.balance(bed.uid).await?.as_micros(), 10_000_000);
    assert!(
        !gateway::images::tasks::run_one(&state)
            .await
            .expect("image recovery")
    );
    assert_eq!(
        bed.hits.load(Ordering::SeqCst),
        1,
        "recovery replayed image generation"
    );
    Ok(())
}

async fn image_state(bed: &Bed, database: &str) -> TestResult<gateway::state::AppState> {
    sqlx::query("UPDATE model_pricing SET pricing_mode='per_call',per_call_price_micro=24 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&bed.model).execute(&bed.pg).await?;
    crate::published_pricing::publish(&bed.pg, bed.uid).await;
    let state = gateway::build_state(
        database,
        &std::env::var("OKAPI_REDIS_URL")?,
        "image-window",
        None,
        None,
    )
    .await?;
    state
        .settings_cache
        .insert("image_tasks_enabled".into(), Arc::new(Some(json!(true))))
        .await;
    Ok(state)
}

async fn assert_image_bill_and_content(
    bed: &Bed,
    address: SocketAddr,
    done: &Value,
    reservation: &okapi_ledger::Reservation,
) -> TestResult {
    let bill: (i64, i16, Option<String>) = sqlx::query_as("SELECT amount_micro,pool,source_window FROM billing_records WHERE request_id=$1 AND status=20")
            .bind(reservation.request_id).fetch_one(&bed.pg).await?;
    assert_eq!(bill, (24, 1, reservation.source_window.clone()));
    let path = done["result"]["data"][0]["url"]
        .as_str()
        .ok_or("private image URL")?;
    let image = reqwest::Client::new()
        .get(format!("http://{address}{path}"))
        .bearer_auth(&bed.token)
        .send()
        .await?;
    assert_eq!(image.status(), 200);
    assert_eq!(image.bytes().await?.as_ref(), b"\x89PNG\r\n\x1a\nfixture");
    Ok(())
}

#[tokio::test]
async fn async_image_actual_charge_stays_in_old_subscription() -> TestResult {
    Box::pin(image_case(false, false)).await
}

#[tokio::test]
async fn async_image_recovers_after_replacement_and_redis_loss() -> TestResult {
    Box::pin(image_case(false, true)).await
}

#[tokio::test]
async fn async_image_failure_does_not_reopen_cancelled_subscription() -> TestResult {
    Box::pin(image_case(true, false)).await
}
