use super::*;

#[tokio::test]
async fn image_cost_is_pinned_for_sync_and_async_requests() {
    for asynchronous in [false, true] {
        for (ratio, cost) in [(1250, 100_000), (0, 0)] {
            let mut env = setup().await;
            sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE id=ANY($1)")
                .bind(&env.channels)
                .bind(json!({"relative_cost_milli":ratio}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            let pending = dispatch(&env, asynchronous).await;
            let peer = env.peer().await;
            assert_eq!(peer.body["n"], 2);
            sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE id=ANY($1)")
                .bind(&env.channels)
                .bind(json!({"relative_cost_milli":2500}))
                .execute(&env.state.pg)
                .await
                .unwrap();
            env.state.channel_cost_cache.invalidate_all();
            peer.images(2);
            timeout(WAIT, pending).await.unwrap().unwrap();
            env.assert_money(PRICE * 2, 1).await;
            verify(&env, ratio, cost).await;
        }
    }
}

async fn dispatch(env: &Env, asynchronous: bool) -> tokio::task::JoinHandle<()> {
    if asynchronous {
        env.state
            .settings_cache
            .insert("image_tasks_enabled".into(), Arc::new(Some(json!(true))))
            .await;
        let response = reqwest::Client::new()
            .post(format!(
                "http://{}/v1/images/generations/async",
                env.address
            ))
            .bearer_auth(&env.token)
            .header("idempotency-key", "cost-source")
            .json(&env.body(2))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        let state = env.state.clone();
        tokio::spawn(async move {
            assert!(gateway::images::tasks::run_one(&state).await.unwrap());
        })
    } else {
        let request = env.request(false).json(&env.body(2));
        tokio::spawn(async move {
            let response = request.send().await.unwrap();
            assert_eq!(response.status(), 200);
        })
    }
}

async fn verify(env: &Env, ratio: i64, cost: i64) {
    let row: (i64,i64,i64,Option<i64>,Value,i64,Uuid)=sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,channel_id,request_id FROM billing_records WHERE user_id=$1 AND log_type=2")
        .bind(env.user).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(
        (row.0, row.1, row.2, row.3),
        (PRICE * 2, PRICE * 2, 0, Some(cost))
    );
    assert_eq!(
        row.4["upstream_cost_basis"],
        json!({"version":1,"source":"selected_channel","channel_id":row.5,"relative_cost_milli":ratio,"list_price_micro":PRICE*2})
    );
    let event:Value=sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(row.6.to_string()).fetch_one(&env.state.pg).await.unwrap();
    assert_eq!(event["upstream_cost_known"], true);
    for (field, value) in [
        ("amount_micro", PRICE * 2),
        ("original_amount_micro", PRICE * 2),
        ("discount_micro", 0),
        ("upstream_cost_micro", cost),
    ] {
        assert_eq!(event[field], value);
    }
    assert_eq!(
        serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
        row.4
    );
}
