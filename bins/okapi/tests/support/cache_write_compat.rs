//! Compatible provider counters must reach actual settlement and portal reports.
use super::{Protocol, record, report, request, setup_with_pricing};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

#[tokio::test]
async fn qwen_and_vllm_cache_writes_reach_settlement_for_json_and_sse() {
    for field in ["cache_creation_input_tokens", "created_cache_tokens"] {
        for protocol in [Protocol::Chat, Protocol::Responses] {
            for stream in [false, true] {
                let ttl = field == "cache_creation_input_tokens";
                let mut details = json!({"cached_tokens":60});
                details[field] = json!(20);
                if ttl {
                    details["cache_creation"] = json!({"ephemeral_5m_input_tokens":20});
                }
                let usage = if matches!(protocol, Protocol::Chat) {
                    json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":details})
                } else {
                    json!({"input_tokens":100,"output_tokens":10,"total_tokens":110,"input_tokens_details":details})
                };
                let env = setup_with_pricing(
                    protocol,
                    usage,
                    Some(("1.25", json!({"cache_write_5m":"2"}))),
                )
                .await;
                let response = request(&env, protocol, stream, false).await;
                let status = response.status();
                let body = response.text().await.unwrap();
                assert_eq!(status, 200, "{field} stream={stream}: {body}");
                let row = record(&env).await;
                let amount = if ttl { 520 } else { 460 };
                assert_eq!(row["amount_micro"], amount, "{row}");
                assert_eq!(row["usage"]["prompt_tokens"], 100);
                assert_eq!(row["usage"]["completion_tokens"], 10);
                assert_eq!(row["usage"]["cached_tokens"], 60);
                assert_eq!(row["usage"]["cache_write_tokens"], 20);
                assert_eq!(row["usage"]["cache_write_reported"], true);
                assert_eq!(
                    row["usage"]["cache_write_5m_tokens"],
                    if ttl { json!(20) } else { Value::Null }
                );
                assert_eq!(
                    row["usage"]["cache_write_1h_tokens"],
                    if ttl { json!(0) } else { Value::Null }
                );
                assert_eq!(
                    env.state
                        .ledger
                        .balance(env.user)
                        .await
                        .unwrap()
                        .as_micros(),
                    50_000_000 - amount
                );
                let stat = report(&env, "/api/me/logs/stat").await;
                assert_eq!(stat["cache_write_tokens"], 20, "{stat}");
                assert_eq!(stat["cache_write_samples"], 1);
                assert_eq!(stat["cache_write_ttl_samples"], i64::from(ttl));
                let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
                    .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
                assert_eq!(payload["cache_write_tokens"], 20);
                assert_eq!(payload["cache_write_reported"], true);
                assert_eq!(payload["amount_micro"], amount);
                assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            }
        }
    }
}
