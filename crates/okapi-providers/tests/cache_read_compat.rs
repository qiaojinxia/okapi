use bytes::Bytes;
use okapi_api::UsageProbe;
use okapi_providers::convert::{
    anthropic_to_openai::{OaiStreamToAnthropic, response_openai_to_anthropic},
    openai_to_anthropic::{StreamState, response_anthropic_to_openai, usage_from_anthropic},
    openai_to_gemini::usage_from_gemini,
};
use okapi_providers::responses::usage_from_responses;
use okapi_providers::{
    ChatEvent,
    anthropic::{AnthropicEvent, MetaScanner},
};
use serde_json::{Value, json};

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/cache_compat.json")).unwrap()
}

#[test]
fn chat_and_responses_bridges_produce_identical_usage() {
    for fixture in fixtures() {
        let probe: UsageProbe = serde_json::from_value(fixture["chat"].clone()).unwrap();
        let expected = probe.to_token_usage().unwrap();
        let responses = usage_from_responses(Some(&fixture["responses"]))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(responses, expected, "{}", fixture["name"]);
        let actual = serde_json::to_value(expected).unwrap();
        for (key, value) in fixture["expected"].as_object().unwrap() {
            assert_eq!(actual[key], *value, "{} {key}", fixture["name"]);
        }
    }
}

#[test]
fn normalized_observations_survive_json_and_stream_conversion() {
    for fixture in fixtures() {
        let raw = &fixture["chat"];
        let probe: UsageProbe = serde_json::from_value(raw.clone()).unwrap();
        let expected = probe.to_token_usage().unwrap();
        let body = json!({"model":"fixture","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":raw});
        let (_, converted) = response_openai_to_anthropic(&Bytes::from(body.to_string())).unwrap();
        assert_eq!(converted.unwrap().to_token_usage().unwrap(), expected);
        let mut stream = OaiStreamToAnthropic::new("fixture");
        let mut events = stream.step(Ok(ChatEvent::Data {
            raw: json!({"choices":[],"usage":raw}).to_string(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage: Some(probe),
        }));
        events.extend(stream.step(Ok(ChatEvent::Done)));
        let observed = events
            .into_iter()
            .filter_map(|e| match e.unwrap() {
                ChatEvent::Data {
                    usage: Some(probe), ..
                } => Some(probe),
                _ => None,
            })
            .last()
            .unwrap();
        assert_eq!(
            observed.to_token_usage().unwrap(),
            expected,
            "{}",
            fixture["name"]
        );
    }
}

#[test]
fn raw_native_usage_survives_existing_anthropic_and_gemini_envelopes() {
    for fixture in fixtures().into_iter().filter(|f| f["bridged"] == true) {
        let raw = &fixture["chat"];
        let expected = serde_json::from_value::<UsageProbe>(raw.clone())
            .unwrap()
            .to_token_usage()
            .unwrap();
        let mut nullable = raw.clone();
        nullable["promptTokenCount"] = Value::Null;
        nullable["candidatesTokenCount"] = Value::Null;
        assert_eq!(
            usage_from_gemini(Some(&nullable))
                .unwrap()
                .to_token_usage()
                .unwrap(),
            expected
        );
        for probe in [
            usage_from_anthropic(Some(raw)),
            usage_from_gemini(Some(raw)),
        ] {
            assert_eq!(probe.unwrap().to_token_usage().unwrap(), expected);
        }
        let body = json!({"id":"fixture","content":[],"usage":raw});
        let (body, converted) =
            response_anthropic_to_openai(&Bytes::from(body.to_string())).unwrap();
        assert_eq!(converted.unwrap().to_token_usage().unwrap(), expected);
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            serde_json::from_value::<UsageProbe>(body["usage"].clone())
                .unwrap()
                .to_token_usage()
                .unwrap(),
            expected
        );
        let mut native = MetaScanner::new();
        let mut converted = StreamState::new("fixture");
        let mut gemini = okapi_providers::gemini::MetaScanner::new();
        let mut latest = [None, None, None];
        for (event, data) in [
            ("message_start", json!({"message":{"usage":raw}})),
            ("message_delta", json!({"usage":raw})),
            ("message_delta", json!({"usage":raw})),
            ("message_stop", json!({})),
        ] {
            for (index, events) in [
                native.scan(Ok(AnthropicEvent {
                    event: event.into(),
                    data: data.to_string(),
                })),
                converted.step(Ok(AnthropicEvent {
                    event: event.into(),
                    data: data.to_string(),
                })),
                gemini.scan(Ok(json!({"usageMetadata":raw}).to_string())),
            ]
            .into_iter()
            .enumerate()
            {
                for item in events {
                    if let ChatEvent::Data {
                        usage: Some(probe), ..
                    } = item.unwrap()
                    {
                        latest[index] = Some(probe);
                    }
                }
            }
        }
        for observed in latest {
            assert_eq!(observed.unwrap().to_token_usage().unwrap(), expected);
        }
    }
}

#[test]
fn gemini_envelopes_can_keep_native_cache_fields_with_normalized_totals() {
    for raw in [
        json!({"promptTokenCount":100,"candidatesTokenCount":10,"cachedContentTokenCount":60,"cacheReadInputTokens":60,"cacheWriteInputTokens":20,"cacheDetails":[{"ttl":"5m","inputTokens":20}]}),
        json!({"promptTokenCount":100,"candidatesTokenCount":10,"total_cached_tokens":60}),
    ] {
        let usage = usage_from_gemini(Some(&raw))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!((usage.prompt_tokens, usage.cached_tokens), (100, 60));
        let mut invalid = raw;
        invalid["cachedContentTokenCount"] = json!(59);
        assert!(
            usage_from_gemini(Some(&invalid))
                .unwrap()
                .with_estimates(100, 10)
                .is_err()
        );
    }
    let anthropic = json!({"input_tokens":40,"output_tokens":10,
        "cache_read_input_tokens":60,"cacheReadInputTokens":null,"total_cached_tokens":null});
    let usage = usage_from_anthropic(Some(&anthropic))
        .unwrap()
        .to_token_usage()
        .unwrap();
    assert_eq!((usage.prompt_tokens, usage.cached_tokens), (100, 60));
}

#[test]
fn malformed_and_mixed_bridge_formats_poison_streams() {
    let start = json!({"inputTokens":20,"outputTokens":0,"cacheReadInputTokens":60,"cacheWriteInputTokens":20});
    for invalid in [
        json!({"inputTokens":20,"outputTokens":10,"cacheReadInputTokens":-1}),
        json!({"input_tokens":20,"output_tokens":10}),
        json!({"inputTokens":20,"outputTokens":10,"input_tokens":100}),
    ] {
        let mut scanner = MetaScanner::new();
        for (event, raw) in [
            ("message_start", &start),
            ("message_delta", &invalid),
            ("message_delta", &start),
        ] {
            let data = if event == "message_start" {
                json!({"message":{"usage":raw}})
            } else {
                json!({"usage":raw})
            };
            let observed = scanner.scan(Ok(AnthropicEvent {
                event: event.into(),
                data: data.to_string(),
            }));
            let probe = observed
                .into_iter()
                .find_map(|item| match item.unwrap() {
                    ChatEvent::Data {
                        usage: Some(probe), ..
                    } => Some(probe),
                    _ => None,
                })
                .unwrap();
            if event == "message_delta" {
                assert!(probe.with_estimates(100, 10).is_err());
            }
        }
    }
}
