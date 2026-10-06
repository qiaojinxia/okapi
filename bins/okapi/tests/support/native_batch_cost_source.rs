use super::*;

#[tokio::test]
async fn native_batch_preserves_selected_cost_and_explicit_zero_through_restart() {
    for (ratio, cost) in [(1250, 50_000), (0, 0)] {
        let env = Env::new().await;
        let channel: i64 =
            sqlx::query_scalar("UPDATE channels SET upstream_unit_cost=$1 RETURNING id")
                .bind(json!({"relative_cost_milli":ratio}))
                .fetch_one(&env.state.pg)
                .await
                .unwrap();
        let job = env.submit(2, "cost-source").await;
        sqlx::query("UPDATE channels SET upstream_unit_cost=$1")
            .bind(json!({"relative_cost_milli":5000}))
            .execute(&env.state.pg)
            .await
            .unwrap();
        let restarted = gateway::build_state(
            &env.database,
            &std::env::var("OKAPI_REDIS_URL").unwrap(),
            "cost-replay",
            None,
            None,
        )
        .await
        .unwrap();
        for _ in 0..4 {
            assert!(advance(&env, &restarted, &job).await.unwrap());
        }
        assert_eq!(env.poll(&job).await["status"], "completed");
        let pricing = env.money(&job, PRICE).await;
        assert_eq!(
            pricing["upstream_cost_basis"],
            json!({"version":1,"source":"selected_channel","channel_id":channel,"relative_cost_milli":ratio,"list_price_micro":PRICE,"batch_cost_ratio_milli":500})
        );
        let row: (i64,i64,i64,Option<i64>) = sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro FROM billing_records WHERE request_id=$1")
            .bind(id(&job)).fetch_one(&env.state.pg).await.unwrap();
        assert_eq!(row, (PRICE, PRICE * 2, PRICE, Some(cost)));
        let event: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
            .bind(id(&job).to_string()).fetch_one(&env.state.pg).await.unwrap();
        assert_eq!(event["upstream_cost_known"], true);
        for (field, expected) in [
            ("amount_micro", PRICE),
            ("original_amount_micro", PRICE * 2),
            ("discount_micro", PRICE),
            ("upstream_cost_micro", cost),
        ] {
            assert_eq!(event[field], expected);
        }
        assert_eq!(
            serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
            pricing
        );
        assert!(!advance(&env, &restarted, &job).await.unwrap());
        restarted.pg.close().await;
        env.close().await;
    }
}

async fn advance(
    env: &Env,
    state: &gateway::state::AppState,
    job: &Value,
) -> Result<bool, gateway::error::AppError> {
    sqlx::query("UPDATE image_batches SET next_run_at=now() WHERE id=$1")
        .bind(id(job))
        .execute(&env.state.pg)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), run_one(state, Some(id(job))))
        .await
        .unwrap()
}
