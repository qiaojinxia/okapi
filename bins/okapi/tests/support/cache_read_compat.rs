//! Mock upstreams, actual gateway settlement, balance, outbox and portal statistics.
use super::{Env, Protocol, record, report, request, setup_with_pricing};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../../crates/okapi-providers/tests/fixtures/cache_compat.json"
    ))
    .unwrap()
}

pub(super) fn rates() -> Value {
    json!({"cache_write_5m":"2","cache_write_1h":"3","audio_cache_read":"2",
        "image_cache_read":"1","image_output":"5"})
}

pub(super) async fn verify(env: &Env, fixture: &Value, amount: i64) {
    let row = record(env).await;
    assert_eq!(row["amount_micro"], amount, "{}: {row}", fixture["name"]);
    for (key, expected) in fixture["expected"].as_object().unwrap() {
        assert_eq!(
            row["usage"][key], *expected,
            "{} {key}: {row}",
            fixture["name"]
        );
    }
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - amount
    );
    let stat = report(env, "/api/me/logs/stat").await;
    assert_eq!(stat["records"], 1, "{stat}");
    assert_eq!(stat["prompt_tokens"], fixture["expected"]["prompt_tokens"]);
    assert_eq!(
        stat["completion_tokens"],
        fixture["expected"]["completion_tokens"]
    );
    assert_eq!(stat["cached_tokens"], fixture["expected"]["cached_tokens"]);
    assert_eq!(stat["cache_read_samples"], 1, "{stat}");
    assert_eq!(
        stat["cache_hit_bp"],
        if fixture["expected"]["cached_tokens"] == 0 {
            0
        } else {
            6000
        }
    );
    let writes = fixture["expected"]["cache_write_reported"] == true;
    assert_eq!(stat["cache_write_samples"], i64::from(writes));
    let ttl = fixture["expected"]["cache_write_5m_tokens"].is_number()
        && fixture["expected"]["cache_write_1h_tokens"].is_number();
    assert_eq!(stat["cache_write_ttl_samples"], i64::from(ttl));
    assert_eq!(
        stat["cache_write_tokens"],
        if writes {
            fixture["expected"]["cache_write_tokens"].clone()
        } else {
            Value::Null
        }
    );
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
    for key in [
        "cached_tokens",
        "cache_read_reported",
        "cache_write_tokens",
        "cache_write_reported",
        "audio_prompt_tokens",
        "reasoning_tokens",
    ] {
        assert_eq!(payload[key], row["usage"][key], "{key}: {payload}");
    }
    assert_eq!(payload["amount_micro"], amount);
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn compatible_reads_and_writes_reach_logs_and_settlement_for_chat_and_responses() {
    for fixture in fixtures() {
        for protocol in [Protocol::Chat, Protocol::Responses] {
            for stream in [false, true] {
                let raw = if matches!(protocol, Protocol::Chat) {
                    &fixture["chat"]
                } else {
                    &fixture["responses"]
                };
                let env = setup_with_pricing(protocol, raw.clone(), Some(("1.25", rates()))).await;
                let response = request(&env, protocol, stream, false).await;
                let status = response.status();
                let body = response.text().await.unwrap();
                assert_eq!(
                    status, 200,
                    "{} {protocol:?} stream={stream}: {body}",
                    fixture["name"]
                );
                assert!(!body.contains("upstream_error"), "{body}");
                verify(&env, &fixture, fixture["amount_micro"].as_i64().unwrap()).await;
            }
        }
    }
}

#[tokio::test]
async fn native_usage_kept_by_bridges_reaches_existing_anthropic_and_gemini_paths() {
    for fixture in fixtures().into_iter().filter(|f| f["bridged"] == true) {
        for protocol in [Protocol::Anthropic, Protocol::Gemini] {
            for stream in [false, true] {
                let env =
                    setup_with_pricing(protocol, fixture["chat"].clone(), Some(("1.25", rates())))
                        .await;
                let response =
                    request(&env, protocol, stream, matches!(protocol, Protocol::Gemini)).await;
                let status = response.status();
                let body = response.text().await.unwrap();
                assert_eq!(
                    status, 200,
                    "{} {protocol:?} stream={stream}: {body}",
                    fixture["name"]
                );
                assert!(!body.contains("upstream_error"), "{body}");
                // Anthropic fixtures intentionally use input ratio 1 / output 2;
                // the other fixtures use input 2 / output 4.
                let amount = if matches!(protocol, Protocol::Anthropic) {
                    if fixture["name"] == "bedrock_native_usage" {
                        236
                    } else {
                        540
                    }
                } else {
                    fixture["amount_micro"].as_i64().unwrap()
                };
                verify(&env, &fixture, amount).await;
            }
        }
    }
}

#[tokio::test]
async fn conflicting_or_malformed_cache_counters_release_reservations_without_estimation() {
    for raw in [
        json!({"prompt_tokens":100,"completion_tokens":10,"prompt_cache_hit_tokens":60,
            "prompt_tokens_details":{"cached_tokens":0}}),
        json!({"prompt_tokens":100,"completion_tokens":10,"prompt_tokens_details":{
            "cached_tokens":60,"audio_tokens":40,"audio_cached_tokens":41}}),
        json!({"inputTokens":20,"outputTokens":10,"cacheReadInputTokens":60,"cacheWriteInputTokens":20,
            "cacheDetails":[{"ttl":"5m","inputTokens":19}]}),
    ] {
        for stream in [false, true] {
            let env =
                setup_with_pricing(Protocol::Chat, raw.clone(), Some(("1.25", rates()))).await;
            let response = request(&env, Protocol::Chat, stream, false).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, if stream { 200 } else { 502 }, "{body}");
            assert!(body.contains("upstream_error"), "{body}");
            assert_eq!(record(&env).await["amount_micro"], 0);
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
        }
    }
}
