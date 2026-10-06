//! Compatibility gaps found in New API / Sub2API, through actual settlement.
use super::{
    Protocol,
    cache_read_compat::{rates, verify},
    record, request, setup_with_pricing,
};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

fn expected(writes: bool) -> Value {
    json!({"name":"cache_aliases","expected":{"prompt_tokens":100,"completion_tokens":10,
        "cached_tokens":60,"cache_read_reported":true,"cache_write_tokens":if writes {20} else {0},
        "cache_write_reported":writes,"audio_prompt_tokens":0,"reasoning_tokens":0}})
}

#[tokio::test]
async fn cache_aliases_settle_chat_responses_and_mixed_messages_without_double_billing() {
    for (protocol, raw, amount) in [
        (
            Protocol::Chat,
            json!({"prompt_tokens":100,"completion_tokens":10,"cache_read_tokens":60,"cache_creation_tokens":20}),
            460,
        ),
        (
            Protocol::Chat,
            json!({"prompt_tokens":100,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":60,"cached_creation_tokens":20}}),
            460,
        ),
        (
            Protocol::Responses,
            json!({"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":60,"cache_write_input_tokens":20}),
            460,
        ),
        (
            Protocol::Responses,
            json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":60,"cache_creation_tokens":20}}),
            460,
        ),
        (
            Protocol::Anthropic,
            json!({"input_tokens":100,"prompt_tokens":100,"output_tokens":10,"prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":40,"cache_write_tokens":20}),
            // 20×2 regular + 60×1 read + 20×2.5 write + 10×4 output.
            190,
        ),
        (
            Protocol::Anthropic,
            json!({"input_tokens":20,"output_tokens":10,"cached_tokens":60,"cached_creation_tokens":20}),
            190,
        ),
    ] {
        for stream in [false, true] {
            let env = setup_with_pricing(protocol, raw.clone(), Some(("1.25", rates()))).await;
            let response = request(&env, protocol, stream, false).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{protocol:?} stream={stream}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            verify(&env, &expected(true), amount).await;
        }
    }
}

#[tokio::test]
async fn cache_envelopes_and_output_only_messages_deltas_reach_logs_and_balance() {
    let axes = json!({"prompt_tokens":100,"completion_tokens":10});
    for raw in [
        json!({"final":axes,"envelope":{"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop","usage":{"cached_tokens":60}}]},"envelope_updates":[{"choices":[],"usage":axes},{"choices":[{"delta":{},"usage":{"cached_tokens":60}}]}]}),
        json!({"final":axes,"envelope":{"timings":{"cache_n":60}},"envelope_updates":[{"usage":axes,"timings":{"cache_n":60}},{"usage":{"completion_tokens":10}}]}),
    ] {
        for stream in [false, true] {
            let env =
                setup_with_pricing(Protocol::Chat, raw.clone(), Some(("1.25", rates()))).await;
            let response = request(&env, Protocol::Chat, stream, false).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{body}");
            assert!(!body.contains("upstream_error"), "{body}");
            verify(&env, &expected(false), 440).await;
        }
    }
    let raw = json!({"final":{"input_tokens":40,"output_tokens":10,"cached_tokens":60},"start":{"input_tokens":40,"output_tokens":0,"cached_tokens":60},"updates":[{"output_tokens":5},{"output_tokens":10}]});
    for stream in [false, true] {
        let env =
            setup_with_pricing(Protocol::Anthropic, raw.clone(), Some(("1.25", rates()))).await;
        let response = request(&env, Protocol::Anthropic, stream, false).await;
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, 200, "{body}");
        assert!(!body.contains("upstream_error"), "{body}");
        // 40×2 regular + 60×1 read + 10×4 output.
        verify(&env, &expected(false), 180).await;
    }
}

#[tokio::test]
async fn contradictory_aliases_and_envelopes_are_unbillable_for_json_and_sse() {
    for (protocol, raw) in [
        (
            Protocol::Chat,
            json!({"prompt_tokens":100,"completion_tokens":10,"cache_write_tokens":20,"cached_creation_tokens":0}),
        ),
        (
            Protocol::Responses,
            json!({"input_tokens":100,"output_tokens":10,"cache_read_tokens":60,"input_tokens_details":{"cached_tokens":0}}),
        ),
        (
            Protocol::Anthropic,
            json!({"input_tokens":41,"prompt_tokens":100,"output_tokens":10,"prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":40}),
        ),
        (
            Protocol::Chat,
            json!({"final":{"prompt_tokens":100,"completion_tokens":10,"cached_tokens":60},"envelope":{"timings":{"cache_n":0}},"envelope_updates":[{"usage":{"prompt_tokens":100,"completion_tokens":10,"cached_tokens":60},"timings":{"cache_n":0}}]}),
        ),
        (
            Protocol::Chat,
            json!({"final":{"prompt_tokens":100,"completion_tokens":10},"envelope":{"timings":{"cache_n":"60"}},"envelope_updates":[{"usage":{"prompt_tokens":100,"completion_tokens":10},"timings":{"cache_n":"60"}}]}),
        ),
    ] {
        for stream in [false, true] {
            let env = setup_with_pricing(protocol, raw.clone(), Some(("1.25", rates()))).await;
            let response = request(&env, protocol, stream, false).await;
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
