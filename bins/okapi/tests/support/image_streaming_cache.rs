use super::super::cache_billing::{assert_cache, cache_env, cached_usage};
use super::*;

#[tokio::test]
async fn partial_cache_reporting_does_not_become_complete_after_per_image_sum() {
    for missing_first in [false, true] {
        let mut env = cache_env().await;
        sqlx::query("UPDATE channels SET settings=jsonb_set(settings,'{image_stream_usage}','\"per_image\"') WHERE id=ANY($1)")
            .bind(&env.channels).execute(&env.state.pg).await.unwrap();
        let known = cached_usage(100);
        let mut missing = known.clone();
        let details = missing["input_tokens_details"].as_object_mut().unwrap();
        details.remove("cached_tokens");
        details.remove("cached_tokens_details");
        let values = if missing_first {
            [missing, known]
        } else {
            [known, missing]
        };
        let call = launch(env.request(false).json(&body(&env, 2)));
        let tx = stream(env.peer().await);
        for value in values {
            send(&tx, completed(false, Some(value))).await;
        }
        drop(tx);
        let response = finish(call, 200).await;
        let record = env.record(&response).await;
        assert_eq!(record["prompt_tokens"], 200);
        assert_eq!(record["cached_tokens"], 50);
        let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1 ORDER BY id DESC LIMIT 1")
            .bind(record["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
        assert_eq!(payload["cache_read_reported"], false);
        assert_eq!(payload["cache_write_reported"], false);
        assert!(!response.text().await.unwrap().contains("event: error"));
    }
}

#[tokio::test]
async fn cached_images_cumulative_and_per_image_modes_preserve_subsets() {
    for per_image in [false, true] {
        let mut env = cache_env().await;
        if per_image {
            sqlx::query("UPDATE channels SET settings=jsonb_set(settings,'{image_stream_usage}','\"per_image\"') WHERE id=ANY($1)")
                .bind(&env.channels).execute(&env.state.pg).await.unwrap();
        }
        let call = launch(env.request(false).json(&body(&env, 2)));
        let tx = stream(env.peer().await);
        send(&tx, completed(false, Some(cached_usage(100)))).await;
        send(
            &tx,
            completed(false, Some(cached_usage(if per_image { 100 } else { 200 }))),
        )
        .await;
        drop(tx);
        let response = finish(call, 200).await;
        assert_cache(
            &env,
            &env.record(&response).await,
            if per_image { 2 } else { 1 },
            200,
        )
        .await;
        let text = response.text().await.unwrap();
        assert!(!text.contains("event: error"));
        assert_eq!(text.matches("event: image_generation.completed").count(), 2);
        env.assert_money(if per_image { 6925 } else { 6462 }, 1)
            .await;
    }
}

#[tokio::test]
async fn decreasing_cache_subset_keeps_only_verified_prefix() {
    let mut env = cache_env().await;
    let call = launch(env.request(false).json(&body(&env, 2)));
    let tx = stream(env.peer().await);
    send(&tx, completed(false, Some(cached_usage(100)))).await;
    let mut regressed = cached_usage(200);
    regressed["input_tokens_details"]["cached_tokens"] = json!(40);
    regressed["input_tokens_details"]["cached_tokens_details"]["image_tokens"] = json!(30);
    send(&tx, completed(false, Some(regressed))).await;
    drop(tx);
    let response = finish(call, 200).await;
    assert_cache(&env, &env.record(&response).await, 1, 100).await;
    let text = response.text().await.unwrap();
    assert!(text.contains("event: error"));
    assert_eq!(text.matches("event: image_generation.completed").count(), 1);
    env.assert_money(3462, 1).await;
}
