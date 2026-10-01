//! Cache-aware Images receipts, rejection boundaries and output modality separation.
use super::token_billing::{assert_usage, response, token_env, usage};
use super::*;

pub(super) async fn cache_env() -> Env {
    let env = token_env().await;
    sqlx::query("UPDATE model_pricing SET completion_ratio=4,cache_ratio=0.25,cache_write_ratio=1.25,modality_ratios=$2 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&env.model).bind(json!({"image_cache_read":"0.4","image_cache_write":"2","image_output":"6"}))
        .execute(&env.state.pg).await.unwrap();
    published_pricing::publish(&env.state.pg, env.user).await;
    env.state.pricebook.replace(
        gateway::pricing_loader::load_pricebook(&env.state.pg)
            .await
            .unwrap(),
    );
    env
}
pub(super) fn cached_usage(output: u32) -> Value {
    let mut value = usage(20, 80, output);
    value["input_tokens_details"]["cached_tokens"] = json!(50);
    value["input_tokens_details"]["cached_tokens_details"] =
        json!({"text_tokens":10,"image_tokens":40});
    value
}
pub(super) async fn assert_cache(env: &Env, record: &Value, multiple: u32, output: u32) {
    assert_usage(env, record, 20 * multiple, 80 * multiple, output).await;
    assert_eq!(record["cached_tokens"], 50 * multiple);
    let snapshot = &record["pricing_snapshot"];
    assert_eq!(
        snapshot["image_cache_usage"]["read_text_tokens"],
        10 * multiple
    );
    assert_eq!(
        snapshot["image_cache_usage"]["read_image_tokens"],
        40 * multiple
    );
    assert_eq!(
        snapshot["modality_ratios"]["image_cache_read"].to_string(),
        "0.4"
    );
    assert_eq!(
        snapshot["cache_read_modalities"],
        json!({"audio_tokens":0,"image_tokens":40*multiple})
    );
}

#[tokio::test]
async fn cached_image_prices_and_unique_partial_splits_match_exact_receipts() {
    let mut env = cache_env().await;
    for (i, missing) in [
        None,
        Some("cached_tokens"),
        Some("text_tokens"),
        Some("image_tokens"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut value = cached_usage(200);
        if let Some(field) = missing {
            let details = &mut value["input_tokens_details"];
            if field == "cached_tokens" {
                details.as_object_mut().unwrap().remove(field);
            } else {
                details["cached_tokens_details"]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            }
        }
        let call = launch(env.request(false).json(&env.body(2)));
        env.peer().await.raw(200, response(value, 2).to_string());
        let reply = finish(call, 200).await;
        let record = env.record(&reply).await;
        assert_cache(&env, &record, 1, 200).await;
        env.assert_money(
            6462 * i64::try_from(i + 1).unwrap(),
            i64::try_from(i + 1).unwrap(),
        )
        .await;
        reply.bytes().await.unwrap();
    }
}

#[tokio::test]
async fn ambiguous_or_invalid_cache_matrix_refunds_without_retry() {
    let mut env = cache_env().await;
    for details in [
        json!({"text_tokens":20,"image_tokens":80,"cached_tokens":50}),
        json!({"text_tokens":20,"image_tokens":80,"cached_tokens":50,"cached_tokens_details":{"text_tokens":11,"image_tokens":40}}),
        json!({"text_tokens":20,"image_tokens":80,"cached_tokens":50,"cached_tokens_details":{"text_tokens":30,"image_tokens":20}}),
        json!({"text_tokens":20,"image_tokens":80,"cached_tokens":50,"cached_tokens_details":{"image_tokens":51}}),
        json!({"text_tokens":20,"image_tokens":80,"cached_tokens_details":{"image_tokens":40}}),
        json!({"text_tokens":20,"image_tokens":80,"cached_tokens":50,"cached_tokens_details":{"text_tokens":10,"image_tokens":40,"audio_tokens":1}}),
        json!({"text_tokens":20,"image_tokens":80,"cached_tokens":50,"cached_tokens_details":{"text_tokens":10,"image_tokens":40},"cache_write_tokens":51,"cache_write_tokens_details":{"text_tokens":10,"image_tokens":41}}),
    ] {
        let mut value = usage(20, 80, 200);
        value["input_tokens_details"] = details;
        let hits = env.hits.load(Ordering::SeqCst);
        let call = launch(env.request(false).json(&env.body(1)));
        env.peer().await.raw(200, response(value, 1).to_string());
        finish(call, 502).await.bytes().await.unwrap();
        env.assert_money(0, 0).await;
        assert_eq!(env.hits.load(Ordering::SeqCst), hits + 1);
    }
}

#[tokio::test]
async fn cache_writes_and_mixed_output_keep_pg_and_outbox_in_agreement() {
    let mut env = cache_env().await;
    let mut value = cached_usage(200);
    value["input_tokens_details"]["cache_write_tokens"] = json!(20);
    value["input_tokens_details"]["cache_write_tokens_details"] =
        json!({"text_tokens":4,"image_tokens":16});
    value["output_tokens_details"] = json!({"text_tokens":20,"image_tokens":180});
    let mut request = env.body(1);
    request["images"] = json!([{"image_url":"data:image/png;base64,iVBORw0KGgo="}]);
    let call = launch(env.request(true).json(&request));
    env.peer().await.raw(200, response(value, 1).to_string());
    let reply = finish(call, 200).await;
    let record = env.record(&reply).await;
    // text 6*5; image 24*8; read 10*1.25+40*2; write 4*6.25+16*10; out 20*20+180*30.
    env.assert_money(6299, 1).await;
    assert_eq!(
        record["pricing_snapshot"]["image_usage"],
        json!({"input_text_tokens":20,"input_image_tokens":80,"output_text_tokens":20,"output_image_tokens":180})
    );
    assert_eq!(
        record["pricing_snapshot"]["image_cache_usage"]["write_image_tokens"],
        16
    );
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1 ORDER BY id DESC LIMIT 1")
        .bind(record["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
    for field in [
        "amount_micro",
        "original_amount_micro",
        "discount_micro",
        "upstream_cost_micro",
        "cached_tokens",
        "completion_tokens",
        "prompt_tokens",
    ] {
        assert_eq!(payload[field], record[field]);
    }
    assert_eq!(payload["cache_write_tokens"], 20);
    assert_eq!(
        serde_json::from_str::<Value>(payload["ratio_snapshot"].as_str().unwrap()).unwrap(),
        record["pricing_snapshot"]
    );
}
