//! Same upstream token fixture through Chat, Responses and Gemini, JSON and SSE.
//! Retained server-tool statistics live in `gateway_server_tool_statistics` (split for the per-suite deadline).
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

#[path = "../../../crates/okapi-gateway/tests/support/published_pricing.rs"]
mod published_pricing;

#[allow(dead_code)] // shared harness: each suite uses a different subset
#[path = "support/usage_modalities.rs"]
mod usage_modalities;
use usage_modalities::*;

#[path = "support/server_tool_prices.rs"]
mod server_tool_prices;

#[path = "support/anthropic_usage.rs"]
mod anthropic_usage;

#[path = "support/server_tools.rs"]
mod server_tools;

#[path = "support/server_tool_fees.rs"]
mod server_tool_fees;

#[path = "support/server_tool_admission.rs"]
mod server_tool_admission;

#[path = "support/server_tool_scope.rs"]
mod server_tool_scope;

#[path = "support/server_tool_routing.rs"]
mod server_tool_routing;

#[path = "support/native_gemini_tools.rs"]
mod native_gemini_tools;

#[path = "support/server_tool_followups.rs"]
mod server_tool_followups;

#[path = "support/native_cost_coverage.rs"]
mod native_cost_coverage;

#[path = "support/cost_source.rs"]
mod cost_source;

#[path = "support/refund_cost_source.rs"]
mod refund_cost_source;

#[path = "support/reservation_provenance.rs"]
mod reservation_provenance;

#[path = "support/cache_ttl_billing.rs"]
mod cache_ttl_billing;

#[path = "support/cache_write_compat.rs"]
mod cache_write_compat;

#[path = "support/cache_read_compat.rs"]
mod cache_read_compat;

#[path = "support/cache_aliases.rs"]
mod cache_aliases;

#[path = "support/usage_provenance.rs"]
mod usage_provenance;

async fn verify(env: &Env) {
    let row = record(env).await;
    assert_eq!(row["amount_micro"], 27_900, "{row}");
    let usage = &row["usage"];
    assert_eq!(
        usage["reported_details"]["prompt"],
        json!({"audio":true,"image":true})
    );
    assert_eq!(
        usage["reported_details"]["completion"],
        json!({"audio":true,"image":true})
    );
    assert_eq!(usage["reported_details"]["reasoning"], true);
    let observation = report(env, "/api/me/logs/stat").await;
    assert_eq!(
        observation["token_detail_observations"]["image_completion_tokens"]["tokens"],
        200
    );
    assert_eq!(
        observation["token_detail_observations"]["image_completion_tokens"]["coverage_bp"],
        10000
    );
    for (name, expected) in [
        ("prompt_tokens", 1000),
        ("completion_tokens", 400),
        ("cached_tokens", 300),
        ("reasoning_tokens", 20),
        ("audio_prompt_tokens", 350),
        ("image_prompt_tokens", 200),
        ("audio_completion_tokens", 100),
        ("image_completion_tokens", 200),
    ] {
        assert_eq!(usage[name], expected, "{name}: {row}");
    }
    assert_eq!(
        usage["cache_read_modalities"],
        json!({"audio_tokens":150,"image_tokens":100})
    );
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - 27_900
    );
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
    for name in [
        "reported_details",
        "prompt_tokens",
        "completion_tokens",
        "cached_tokens",
        "reasoning_tokens",
    ] {
        assert_eq!(payload[name], usage[name]);
    }
    for name in ["amount_micro", "original_amount_micro", "discount_micro"] {
        assert_eq!(payload[name], row[name]);
    }
    if let Some(ch) = &env.state.ch {
        ch.ensure_schema().await.unwrap();
        for _ in 0..100 {
            if okapi::worker::chsink::process_once(&env.state.pg, ch)
                .await
                .unwrap()
                == 0
            {
                break;
            }
        }
        let stats = report(env, "/api/me/stats/breakdown?days=1").await;
        assert_eq!(stats["total"]["requests"], 1);
        assert_eq!(stats["total"]["tokens"], 1400);
        assert_eq!(stats["total"]["cache_hit_bp"], 3000);
    } else {
        eprintln!("SKIP: modal usage CH assertions require OKAPI_CLICKHOUSE_URL");
    }
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn equivalent_protocol_usage_matches_money_ledger_outbox_and_statistics() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Gemini] {
        for stream in [false, true] {
            let env = setup(protocol, protocol.fixture()).await;
            let resp = request(&env, protocol, stream, false).await;
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(status, 200, "{protocol:?} {stream}: {body}");
            assert!(!body.contains("\"code\":\"upstream_error\""), "{body}");
            verify(&env).await;
        }
    }
}

#[tokio::test]
async fn native_gemini_usage_matches_converted_usage() {
    for stream in [false, true] {
        let env = setup(Protocol::Gemini, Protocol::Gemini.fixture()).await;
        let resp = request(&env, Protocol::Gemini, stream, true).await;
        let status = resp.status();
        let body = resp.text().await.unwrap();
        assert_eq!(status, 200, "{body}");
        verify(&env).await;
    }
}

#[tokio::test]
async fn malformed_usage_refunds_without_estimation_or_upstream_replay() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Gemini] {
        for stream in [false, true] {
            let mut usage = protocol.fixture();
            let key = match protocol {
                Protocol::Chat => "prompt_tokens",
                Protocol::Responses | Protocol::Anthropic => "input_tokens",
                Protocol::Gemini => "promptTokenCount",
            };
            usage[key] = json!(-1);
            let env = setup(protocol, usage).await;
            let resp = request(&env, protocol, stream, false).await;
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(
                status,
                if stream { 200 } else { 502 },
                "{protocol:?}: {body}"
            );
            assert!(body.contains("upstream_error"), "{protocol:?}: {body}");
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 0, "{row}");
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

#[tokio::test]
async fn converted_missing_usage_estimates_but_explicit_zero_remains_zero() {
    for stream in [false, true] {
        for zero in [false, true] {
            for ingress in [Protocol::Chat, Protocol::Responses, Protocol::Gemini] {
                let usage = if zero {
                    json!({"prompt_tokens":0,"completion_tokens":0})
                } else {
                    Value::Null
                };
                let env = setup(Protocol::Chat, usage).await;
                let resp =
                    request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
                let status = resp.status();
                let body = resp.text().await.unwrap();
                assert_eq!(status, 200, "{ingress:?} {stream}: {body}");
                let row = record(&env).await;
                let amount = row["amount_micro"].as_i64().unwrap();
                let input = row["usage"]["prompt_tokens"].as_i64().unwrap();
                if zero {
                    assert_eq!((amount, input), (0, 0));
                } else {
                    assert!(amount > 0 && input > 0, "{row}");
                }
                assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            }
        }
    }
}
