use super::{Env, Protocol, record, report, request, setup};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

fn partial(protocol: Protocol, input: bool) -> Value {
    match (protocol, input) {
        (Protocol::Chat, true) => {
            json!({"prompt_tokens":97,"prompt_tokens_details":{"cached_tokens":20}})
        }
        (Protocol::Chat, false) => json!({"completion_tokens":99}),
        (Protocol::Responses, true) => {
            json!({"input_tokens":97,"input_tokens_details":{"cached_tokens":20}})
        }
        (Protocol::Responses | Protocol::Anthropic, false) => json!({"output_tokens":99}),
        (Protocol::Gemini, true) => json!({"promptTokenCount":97,"cachedContentTokenCount":20}),
        (Protocol::Gemini, false) => json!({"candidatesTokenCount":99}),
        (Protocol::Anthropic, true) => json!({"input_tokens":77,"cache_read_input_tokens":20}),
    }
}

async fn admin(env: &Env) {
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(env.user)
        .execute(&env.state.pg)
        .await
        .unwrap();
}

async fn verify_sources(env: &Env, row: &Value, upstream: Value, sources: (&str, &str)) {
    let usage = &row["usage"];
    assert_eq!(usage["upstream_usage"], upstream, "{row}");
    assert_eq!(usage["prompt_source"], sources.0, "{row}");
    assert_eq!(usage["completion_source"], sources.1, "{row}");
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
    for key in ["upstream_usage", "prompt_source", "completion_source"] {
        assert_eq!(payload[key], usage[key]);
    }
    let detail: Value =
        sqlx::query_scalar("SELECT usage_details FROM billing_records WHERE request_id::text=$1")
            .bind(row["request_id"].as_str().unwrap())
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(detail["tokens"]["upstream_usage"], upstream);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - row["amount_micro"].as_i64().unwrap()
    );
    let ch = env
        .state
        .ch
        .as_ref()
        .expect("provenance requires isolated ClickHouse");
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
    let logs = report(
        env,
        &format!(
            "/admin/logs?request_id={}",
            row["request_id"].as_str().unwrap()
        ),
    )
    .await;
    let rows = logs["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{logs}");
    for key in [
        "prompt_tokens",
        "completion_tokens",
        "upstream_usage",
        "prompt_source",
        "completion_source",
    ] {
        assert_eq!(rows[0]["usage"][key], usage[key], "{key}: {logs}");
    }
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn partial_usage_retains_known_counts_and_sources_through_protocol_hops() {
    for (upstream, ingress) in [
        (Protocol::Chat, Protocol::Chat),
        (Protocol::Chat, Protocol::Anthropic),
        (Protocol::Chat, Protocol::Responses),
        (Protocol::Chat, Protocol::Gemini),
        (Protocol::Responses, Protocol::Responses),
        (Protocol::Gemini, Protocol::Gemini),
        (Protocol::Gemini, Protocol::Chat),
        (Protocol::Anthropic, Protocol::Anthropic),
        (Protocol::Anthropic, Protocol::Chat),
        (Protocol::Anthropic, Protocol::Responses),
        (Protocol::Anthropic, Protocol::Gemini),
    ] {
        for stream in [false, true] {
            for input in [false, true] {
                let env = setup(upstream, partial(upstream, input)).await;
                admin(&env).await;
                let resp =
                    request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
                let status = resp.status();
                let body = resp.text().await.unwrap();
                assert_eq!(
                    status, 200,
                    "{upstream:?} -> {ingress:?} {stream} {input}: {body}"
                );
                assert!(!body.contains("upstream_error"), "{body}");
                let row = record(&env).await;
                let prompt = row["usage"]["prompt_tokens"].as_i64().unwrap();
                let output = row["usage"]["completion_tokens"].as_i64().unwrap();
                let known = if input {
                    json!({"prompt_tokens":97,"completion_tokens":null})
                } else {
                    json!({"prompt_tokens":null,"completion_tokens":99})
                };
                if input {
                    assert_eq!(prompt, 97, "{upstream:?} -> {ingress:?} {stream}: {row}");
                    assert_eq!(row["usage"]["cached_tokens"], 20);
                    assert!(output > 0);
                } else {
                    assert_eq!(output, 99);
                    assert!(prompt > 0);
                }
                let (base, completion) = if matches!(upstream, Protocol::Anthropic) {
                    (2, 4)
                } else {
                    (4, 16)
                };
                let expected =
                    prompt * base + output * completion - if input { 10 * base } else { 0 };
                assert_eq!(row["amount_micro"], expected, "{row}");
                let sources = if input {
                    ("upstream", "estimated")
                } else {
                    ("estimated", "upstream")
                };
                verify_sources(&env, &row, known, sources).await;
            }
        }
    }
}

#[tokio::test]
async fn complete_zero_missing_and_local_override_are_distinct_in_both_log_apis() {
    for (raw, trust, sources, upstream) in [
        (
            json!({"prompt_tokens":0,"completion_tokens":0}),
            true,
            ("upstream", "upstream"),
            json!({"prompt_tokens":0,"completion_tokens":0}),
        ),
        (
            Value::Null,
            true,
            ("estimated", "estimated"),
            json!({"prompt_tokens":null,"completion_tokens":null}),
        ),
        (
            json!({"prompt_tokens":0,"completion_tokens":99}),
            false,
            ("local_override", "upstream"),
            json!({"prompt_tokens":0,"completion_tokens":99}),
        ),
    ] {
        let env = setup(Protocol::Chat, raw).await;
        admin(&env).await;
        sqlx::query("UPDATE channels SET trust_upstream_usage=$2 WHERE name=$1")
            .bind(&env.model)
            .bind(trust)
            .execute(&env.state.pg)
            .await
            .unwrap();
        let resp = request(&env, Protocol::Chat, false, false).await;
        assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
        let row = record(&env).await;
        verify_sources(&env, &row, upstream, sources).await;
    }
}

#[tokio::test]
async fn stream_axes_arriving_separately_are_retained_without_summing_replays() {
    for ingress in [
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
        Protocol::Anthropic,
    ] {
        for reverse in [false, true] {
            let mut updates = vec![
                partial(Protocol::Chat, true),
                partial(Protocol::Chat, false),
            ];
            if reverse {
                updates.reverse();
            }
            updates.push(updates[1].clone());
            let env = setup(Protocol::Chat, json!({"updates":updates})).await;
            admin(&env).await;
            let resp = request(&env, ingress, true, matches!(ingress, Protocol::Gemini)).await;
            assert_eq!(resp.status(), 200);
            let body = resp.text().await.unwrap();
            assert!(!body.contains("upstream_error"), "{body}");
            let row = record(&env).await;
            assert_eq!(row["usage"]["prompt_tokens"], 97);
            assert_eq!(row["usage"]["completion_tokens"], 99);
            assert_eq!(row["amount_micro"], 1932);
            verify_sources(
                &env,
                &row,
                json!({"prompt_tokens":97,"completion_tokens":99}),
                ("upstream", "upstream"),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn invalid_earlier_stream_usage_cannot_be_replaced_with_a_valid_bill() {
    for ingress in [
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
        Protocol::Anthropic,
    ] {
        let env = setup(
            Protocol::Chat,
            json!({"updates":[{"prompt_tokens":-1}, {"prompt_tokens":97,"completion_tokens":99}]}),
        )
        .await;
        let resp = request(&env, ingress, true, matches!(ingress, Protocol::Gemini)).await;
        assert_eq!(resp.status(), 200);
        assert!(resp.text().await.unwrap().contains("upstream_error"));
        let row = record(&env).await;
        assert_eq!(row["amount_micro"], 0);
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

#[tokio::test]
async fn legacy_records_and_outbox_without_provenance_remain_unknown() {
    let env = setup(
        Protocol::Chat,
        json!({"prompt_tokens":97,"completion_tokens":99}),
    )
    .await;
    admin(&env).await;
    let resp = request(&env, Protocol::Chat, false, false).await;
    assert_eq!(resp.status(), 200);
    let original = record(&env).await;
    let id = original["request_id"].as_str().unwrap();
    // Remove only provenance from this test's own fixture; all amounts/counters remain.
    sqlx::query("UPDATE billing_records SET usage_details=(usage_details-'prompt_source'-'completion_source') #- '{tokens,upstream_usage}' WHERE request_id::text=$1")
        .bind(id).execute(&env.state.pg).await.unwrap();
    sqlx::query("UPDATE billing_outbox SET payload=payload-'prompt_source'-'completion_source'-'upstream_usage' WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(id).execute(&env.state.pg).await.unwrap();
    let row = record(&env).await;
    assert_eq!(row["usage"]["prompt_source"], "unknown");
    assert_eq!(row["usage"]["completion_source"], "unknown");
    assert!(row["usage"]["upstream_usage"].is_null());
    let ch = env.state.ch.as_ref().unwrap();
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
    let logs = report(&env, &format!("/admin/logs?request_id={id}")).await;
    let admin = &logs["data"][0];
    for key in [
        "prompt_tokens",
        "completion_tokens",
        "prompt_source",
        "completion_source",
        "upstream_usage",
    ] {
        assert_eq!(admin["usage"][key], row["usage"][key], "{logs}");
    }
    assert_eq!(admin["amount_micro"], original["amount_micro"]);
    assert_eq!(row["amount_micro"], original["amount_micro"]);
}
