use super::{Env, Protocol, record, report, request, setup};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::atomic::Ordering;

pub(super) fn fixture() -> Value {
    json!({"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":800,
        "cache_creation_input_tokens":100,"cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":0},
        "output_tokens_details":{"thinking_tokens":20}})
}

pub(super) fn response(raw: &Value) -> (Value, String) {
    let usage = raw.get("final").unwrap_or(raw);
    let body = json!({"id":"msg_usage","type":"message","role":"assistant","model":"fixture",
        "content":[{"type":"text","text":"Hello"}],"stop_reason":"end_turn","usage":usage});
    let start = raw.get("start").unwrap_or(usage);
    let updates = raw
        .get("updates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![usage.clone()]);
    let mut events = format!(
        "event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\n",
        json!({"type":"message_start","message":{"id":"msg_usage","type":"message","role":"assistant","model":"fixture","content":[],"usage":start}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}})
    );
    for usage in updates {
        write!(
            events,
            "event: message_delta\ndata: {}\n\n",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":usage})
        )
        .unwrap();
    }
    events.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    (body, events)
}

async fn verify_totals(env: &Env) {
    let row = record(env).await;
    assert_eq!(row["amount_micro"], 1600, "{row}");
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - 1600
    );
    for (key, count) in [
        ("prompt_tokens", 1000),
        ("completion_tokens", 50),
        ("cached_tokens", 800),
        ("cache_write_tokens", 100),
        ("cache_write_5m_tokens", 100),
        ("cache_write_1h_tokens", 0),
        ("reasoning_tokens", 20),
    ] {
        assert_eq!(row["usage"][key], count, "{row}");
    }
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
    for key in [
        "prompt_tokens",
        "completion_tokens",
        "cached_tokens",
        "cache_write_tokens",
        "cache_write_5m_tokens",
        "cache_write_1h_tokens",
        "reasoning_tokens",
    ] {
        assert_eq!(payload[key], row["usage"][key]);
    }
    for key in ["amount_micro", "original_amount_micro", "discount_micro"] {
        assert_eq!(payload[key], row[key]);
    }
    // Upstream cost is intentionally absent from the customer-facing receipt.
    let cost: Option<i64> = sqlx::query_scalar(
        "SELECT upstream_cost_micro FROM billing_records WHERE request_id::text=$1",
    )
    .bind(row["request_id"].as_str().unwrap())
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    assert_eq!(payload["upstream_cost_micro"], json!(cost.unwrap_or(0)));
    assert_eq!(payload["upstream_cost_known"], json!(cost.is_some()));
    let ch = env
        .state
        .ch
        .as_ref()
        .expect("Anthropic statistics require the isolated CH service");
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
    // A running JetStream worker can claim the outbox before the direct drain.
    // Published does not mean CH ingestion has completed; wait for this key's row.
    let mut stats = report(env, "/api/me/stats/breakdown?days=1").await;
    for _ in 0..50 {
        if stats["total"]["requests"] == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        stats = report(env, "/api/me/stats/breakdown?days=1").await;
    }
    let usage_stat = report(env, "/api/me/logs/stat").await;
    assert_eq!(usage_stat["cache_write_5m_tokens"], 100);
    assert_eq!(usage_stat["cache_write_1h_tokens"], 0);
    assert_eq!(usage_stat["cache_write_ttl_samples"], 1);
    assert_eq!(stats["total"]["requests"], 1, "{stats}");
    assert_eq!(stats["total"]["tokens"], 1050, "{stats}");
    assert_eq!(stats["total"]["amount_micro"], 1600, "{stats}");
    assert_eq!(stats["total"]["cache_hit_bp"], 8000, "{stats}");
    for (key, count) in [
        ("prompt_tokens", 1000),
        ("completion_tokens", 50),
        ("cached_tokens", 800),
        ("cache_write_tokens", 100),
        ("reasoning_tokens", 20),
    ] {
        assert_eq!(stats["total"][key], count, "{stats}");
    }
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn anthropic_json_and_cumulative_streams_match_all_ingresses_and_statistics() {
    let final_usage = fixture();
    let mock = json!({"final":final_usage,"start":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":20},
        "updates":[{"output_tokens":10},final_usage,final_usage,null]});
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup(Protocol::Anthropic, mock.clone()).await;
            let response =
                request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{ingress:?}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            verify_totals(&env).await;
        }
    }
}

#[tokio::test]
async fn anthropic_invalid_json_or_stream_usage_refunds_without_replay() {
    let valid = fixture();
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let mock = json!({"final":{"input_tokens":-1,"output_tokens":50},
                "start":{"input_tokens":100,"output_tokens":1},
                "updates":[{"output_tokens":10},{"output_tokens":-1},valid]});
            let env = setup(Protocol::Anthropic, mock).await;
            let response =
                request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(
                status,
                if stream { 200 } else { 502 },
                "{ingress:?}: {body}"
            );
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

#[tokio::test]
async fn anthropic_missing_and_zero_usage_have_distinct_settlement_paths() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            for zero in [false, true] {
                let usage = if zero {
                    json!({"input_tokens":0,"output_tokens":0})
                } else {
                    Value::Null
                };
                let env = setup(Protocol::Anthropic, usage).await;
                let response =
                    request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
                let status = response.status();
                let body = response.text().await.unwrap();
                assert_eq!(status, 200, "{ingress:?}: {body}");
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
