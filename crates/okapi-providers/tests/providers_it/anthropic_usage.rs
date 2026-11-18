use bytes::Bytes;
use okapi_api::UsageProbe;
use okapi_providers::{
    ChatEvent,
    anthropic::{AnthropicEvent, MetaScanner},
    convert::openai_to_anthropic::{
        StreamState, response_anthropic_to_openai, usage_from_anthropic,
    },
};
use serde_json::{Value, json};

fn event(name: &str, value: &Value) -> AnthropicEvent {
    AnthropicEvent {
        event: name.into(),
        data: value.to_string(),
    }
}

fn stream(start: &Value, deltas: Vec<Value>) -> [Option<UsageProbe>; 2] {
    let mut native = MetaScanner::new();
    let mut converted = StreamState::new("fixture");
    let mut latest = [None, None];
    let events = std::iter::once(("message_start", json!({"message":{"usage":start}})))
        .chain(
            deltas
                .into_iter()
                .map(|v| ("message_delta", json!({"usage":v}))),
        )
        .chain(std::iter::once(("message_stop", json!({}))));
    for (name, data) in events {
        for (i, events) in [
            native.scan(Ok(event(name, &data))),
            converted.step(Ok(event(name, &data))),
        ]
        .into_iter()
        .enumerate()
        {
            for e in events {
                if let ChatEvent::Data { usage: Some(u), .. } = e.unwrap() {
                    latest[i] = Some(u);
                }
            }
        }
    }
    latest
}

#[test]
fn json_preserves_cache_totals_reasoning_and_missing_state() {
    let raw = json!({"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":800,
        "cache_creation_input_tokens":100,"cache_creation":{"ephemeral_5m_input_tokens":60,"ephemeral_1h_input_tokens":40},
        "output_tokens_details":{"thinking_tokens":20}});
    let u = usage_from_anthropic(Some(&raw))
        .unwrap()
        .to_token_usage()
        .unwrap();
    assert_eq!(
        (u.prompt_tokens, u.completion_tokens, u.reasoning_tokens),
        (1000, 50, 20)
    );
    assert_eq!(
        (u.cached_tokens, u.cache_write_tokens, u.prompt_uncached()),
        (800, 100, 100)
    );
    assert!(u.cache_read_reported && u.cache_write_reported);
    assert_eq!(
        (u.cache_write_5m_tokens, u.cache_write_1h_tokens),
        (Some(60), Some(40))
    );
    let source = json!({"id":"m","content":[],"usage":raw}).to_string();
    let (body, probe) = response_anthropic_to_openai(&Bytes::from(source)).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["usage"]["total_tokens"], 1050);
    let reparsed: UsageProbe = serde_json::from_value(body["usage"].clone()).unwrap();
    assert_eq!(
        reparsed.to_token_usage().unwrap(),
        u,
        "TTL survives protocol conversion"
    );
    assert_eq!(
        body["usage"]["completion_tokens_details"]["reasoning_tokens"],
        20
    );
    assert_eq!(probe.unwrap().to_token_usage().unwrap(), u);
    for missing in [None, Some(&Value::Null)] {
        assert!(usage_from_anthropic(missing).is_none());
    }
    for body in [json!({"content":[]}), json!({"content":[],"usage":null})] {
        let (body, probe) = response_anthropic_to_openai(&Bytes::from(body.to_string())).unwrap();
        assert!(probe.is_none());
        assert!(serde_json::from_slice::<Value>(&body).unwrap()["usage"].is_null());
    }
}

#[test]
fn malformed_json_usage_is_never_coerced_into_billable_zero() {
    for invalid in [
        json!({}),
        json!([]),
        json!("usage"),
        json!({"input_tokens":100}),
        json!({"output_tokens":100}),
        json!({"input_tokens":-1,"output_tokens":10}),
        json!({"input_tokens":1.5,"output_tokens":10}),
        json!({"input_tokens":"10","output_tokens":10}),
        json!({"input_tokens":0,"output_tokens":4_294_967_296_u64}),
        json!({"input_tokens":2_147_483_647,"output_tokens":0,"cache_read_input_tokens":1}),
        json!({"input_tokens":0,"output_tokens":10,"cache_creation_input_tokens":-1}),
        json!({"input_tokens":0,"output_tokens":10,"output_tokens_details":{"thinking_tokens":11}}),
        json!({"input_tokens":0,"output_tokens":10,"output_tokens_details":{"thinking_tokens":0.5}}),
        json!({"input_tokens":0,"output_tokens":10,"cache_creation_input_tokens":3,"cache_creation":{"ephemeral_1h_input_tokens":4}}),
        json!({"input_tokens":0,"output_tokens":10,"cache_creation":{"ephemeral_5m_input_tokens":5}}),
    ] {
        assert!(
            usage_from_anthropic(Some(&invalid))
                .unwrap()
                .to_token_usage()
                .is_err(),
            "{invalid}"
        );
    }
    let max = usage_from_anthropic(Some(
        &json!({"input_tokens":2_147_483_647,"output_tokens":2_147_483_647}),
    ))
    .unwrap()
    .to_token_usage()
    .unwrap();
    assert_eq!(max.prompt_tokens, 2_147_483_647);
}

#[test]
fn stream_merges_cumulative_updates_and_never_adds_replayed_deltas() {
    let start = json!({"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":20});
    let final_usage = json!({"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":800,
        "cache_creation_input_tokens":100,"output_tokens_details":{"thinking_tokens":20}});
    let results = stream(
        &start,
        vec![
            json!({"output_tokens":10}),
            final_usage.clone(),
            final_usage.clone(),
            Value::Null,
        ],
    );
    for result in results {
        assert_eq!(
            result.unwrap().to_token_usage().unwrap(),
            usage_from_anthropic(Some(&final_usage))
                .unwrap()
                .to_token_usage()
                .unwrap()
        );
    }
}

#[test]
fn stream_retains_ttl_on_output_updates_but_clears_stale_splits() {
    let start = json!({"input_tokens":100,"output_tokens":1,"cache_creation_input_tokens":100,
        "cache_creation":{"ephemeral_5m_input_tokens":60,"ephemeral_1h_input_tokens":40}});
    for result in stream(
        &start,
        vec![json!({"output_tokens":10}), json!({"output_tokens":20})],
    ) {
        let usage = result.unwrap().to_token_usage().unwrap();
        assert_eq!(
            (
                usage.cache_write_tokens,
                usage.cache_write_5m_tokens,
                usage.cache_write_1h_tokens
            ),
            (100, Some(60), Some(40))
        );
    }
    for result in stream(
        &start,
        vec![json!({"output_tokens":20,"cache_creation_input_tokens":120})],
    ) {
        let usage = result.unwrap().to_token_usage().unwrap();
        assert_eq!(usage.cache_write_tokens, 120);
        assert_eq!(
            (usage.cache_write_5m_tokens, usage.cache_write_1h_tokens),
            (None, None)
        );
    }
}

#[test]
fn stream_input_updates_are_used_and_null_cache_fields_preserve_prior_counts() {
    for result in stream(
        &json!({"input_tokens":100,"output_tokens":1,"cache_read_input_tokens":200}),
        vec![
            json!({"output_tokens":20}),
            json!({"input_tokens":80,"cache_read_input_tokens":null,"cache_creation_input_tokens":20}),
        ],
    ) {
        let u = result.unwrap().to_token_usage().unwrap();
        assert_eq!(
            (u.prompt_tokens, u.completion_tokens, u.cache_write_tokens),
            (300, 20, 20)
        );
        assert!(u.cache_read_reported && u.cache_write_reported);
    }
}

#[test]
fn missing_stream_usage_estimates_instead_of_manufacturing_zero_or_initial_output() {
    assert!(
        stream(&Value::Null, vec![Value::Null])
            .into_iter()
            .all(|p| p.is_none())
    );
    for (results, expected, sources) in [
        (
            stream(
                &json!({"input_tokens":100,"output_tokens":1}),
                vec![Value::Null],
            ),
            (100, 23),
            ("upstream", "estimated"),
        ),
        (
            stream(&Value::Null, vec![json!({"output_tokens":10})]),
            (17, 10),
            ("estimated", "upstream"),
        ),
    ] {
        for result in results {
            let usage = result.unwrap().with_estimates(17, 23).unwrap();
            assert_eq!((usage.prompt_tokens, usage.completion_tokens), expected);
            assert_eq!((usage.prompt_source(), usage.completion_source()), sources);
        }
    }
    for result in stream(
        &json!({"input_tokens":0,"output_tokens":0}),
        vec![json!({"output_tokens":0})],
    ) {
        let u = result.unwrap().to_token_usage().unwrap();
        assert_eq!((u.prompt_tokens, u.completion_tokens), (0, 0));
    }
}

#[test]
fn malformed_or_regressing_stream_usage_stays_invalid_after_valid_updates() {
    for invalid in [
        json!({}),
        json!({"output_tokens":-1}),
        json!({"output_tokens":9}),
        json!({"cache_read_input_tokens":2_147_483_647}),
        json!({"output_tokens":"40"}),
        json!({"output_tokens":20,"output_tokens_details":{"thinking_tokens":21}}),
    ] {
        for result in stream(
            &json!({"input_tokens":100,"output_tokens":1}),
            vec![
                json!({"output_tokens":10}),
                invalid.clone(),
                json!({"output_tokens":50}),
            ],
        ) {
            assert!(result.unwrap().to_token_usage().is_err(), "{invalid}");
        }
    }
    for result in stream(
        &json!({"input_tokens":-1}),
        vec![json!({"input_tokens":100,"output_tokens":50})],
    ) {
        assert!(result.unwrap().invalid);
    }
}

#[test]
fn malformed_stream_json_and_duplicate_start_cannot_reset_invalid_usage() {
    for broken in [
        AnthropicEvent {
            event: "message_delta".into(),
            data: "{".into(),
        },
        event(
            "message_start",
            &json!({"message":{"usage":{"input_tokens":100,"output_tokens":1}}}),
        ),
    ] {
        let mut native = MetaScanner::new();
        let mut converted = StreamState::new("fixture");
        let start = json!({"message":{"usage":{"input_tokens":100,"output_tokens":1}}});
        native.scan(Ok(event("message_start", &start)));
        converted.step(Ok(event("message_start", &start)));
        for events in [native.scan(Ok(broken.clone())), converted.step(Ok(broken))] {
            assert!(events.into_iter().any(|e| matches!(
                e.unwrap(),
                ChatEvent::Data {
                    usage: Some(UsageProbe { invalid: true, .. }),
                    ..
                }
            )));
        }
    }
}

#[test]
fn explicit_zero_thinking_survives_conversion_and_absence_stays_unknown() {
    for reported in [false, true] {
        let mut raw = json!({"input_tokens":100,"output_tokens":50});
        if reported {
            raw["output_tokens_details"] = json!({"thinking_tokens":0});
        }
        let probe = usage_from_anthropic(Some(&raw)).unwrap();
        assert_eq!(
            probe
                .to_token_usage()
                .unwrap()
                .reported_details
                .unwrap()
                .reasoning,
            reported
        );
        let chat = json!({"id":"fixture","model":"fixture","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":probe.chat_json()});
        let (wire, _) =
            okapi_providers::convert::anthropic_to_openai::response_openai_to_anthropic(
                &Bytes::from(chat.to_string()),
            )
            .unwrap();
        let converted: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            converted["usage"].get("output_tokens_details").is_some(),
            reported
        );
        if reported {
            assert_eq!(
                converted["usage"]["output_tokens_details"]["thinking_tokens"],
                0
            );
        }
        assert_eq!(
            usage_from_anthropic(Some(&converted["usage"]))
                .unwrap()
                .to_token_usage()
                .unwrap(),
            probe.to_token_usage().unwrap()
        );
    }
}
