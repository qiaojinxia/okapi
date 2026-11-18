use okapi_providers::{
    ChatEvent,
    anthropic::{AnthropicEvent, MetaScanner},
    convert::openai_to_anthropic::usage_from_anthropic,
    responses::usage_from_responses,
};
use serde_json::{Value, json};

#[test]
fn messages_compatible_counts_normalize_inclusive_and_exclusive_inputs() {
    for raw in [
        json!({"input_tokens":40,"output_tokens":10,"cached_tokens":60}),
        json!({"input_tokens":40,"output_tokens":10,"cache_read_tokens":60}),
        json!({"input_tokens":100,"prompt_tokens":100,"output_tokens":10,"prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":40}),
        json!({"input_tokens":40,"prompt_tokens":100,"output_tokens":10,"prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":40}),
        json!({"output_tokens":10,"prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":40}),
    ] {
        let u = usage_from_anthropic(Some(&raw))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(
            (u.prompt_tokens, u.completion_tokens, u.cached_tokens),
            (100, 10, 60),
            "{raw}"
        );
        assert!(u.cache_read_reported);
        assert!(!u.cache_write_reported);
    }
    for field in [
        "cache_creation_tokens",
        "cached_creation_tokens",
        "cache_write_tokens",
        "cache_write_input_tokens",
    ] {
        let mut raw =
            json!({"input_tokens":100,"prompt_tokens":100,"output_tokens":10,"cached_tokens":60});
        raw[field] = json!(20);
        let u = usage_from_anthropic(Some(&raw))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(
            (u.prompt_tokens, u.prompt_uncached(), u.cache_write_tokens),
            (100, 20, 20)
        );
    }
}

#[test]
fn messages_stream_missing_aliases_retain_counts_and_zero_replaces_them() {
    let mut scanner = MetaScanner::new();
    let mut latest = None;
    for (event, usage) in [
        (
            "message_start",
            json!({"input_tokens":40,"output_tokens":0,"cached_tokens":60}),
        ),
        ("message_delta", json!({"output_tokens":5})),
        (
            "message_delta",
            json!({"output_tokens":10,"cache_read_tokens":0,"cache_creation_tokens":0}),
        ),
    ] {
        let data = if event == "message_start" {
            json!({"message":{"usage":usage}})
        } else {
            json!({"usage":usage})
        };
        for item in scanner.scan(Ok(AnthropicEvent {
            event: event.into(),
            data: data.to_string(),
        })) {
            if let ChatEvent::Data {
                usage: Some(probe), ..
            } = item.unwrap()
            {
                latest = Some(probe);
            }
        }
        if event == "message_delta" && usage["output_tokens"] == 5 {
            assert_eq!(latest.unwrap().to_token_usage().unwrap().cached_tokens, 60);
        }
    }
    let u = latest.unwrap().to_token_usage().unwrap();
    assert_eq!(
        (
            u.prompt_tokens,
            u.completion_tokens,
            u.cached_tokens,
            u.cache_write_tokens
        ),
        (40, 10, 0, 0)
    );
    assert!(u.cache_read_reported && u.cache_write_reported);
}

#[test]
fn mixed_messages_conflicts_and_malformed_aliases_cannot_be_estimated() {
    for raw in [
        json!({"input_tokens":41,"prompt_tokens":100,"output_tokens":10,"cached_tokens":60}),
        json!({"prompt_tokens":100,"output_tokens":10,"prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":41}),
        json!({"input_tokens":40,"output_tokens":10,"cache_read_input_tokens":0,"cached_tokens":60}),
        json!({"input_tokens":40,"output_tokens":10,"cache_creation_tokens":-1}),
        json!({"input_tokens":40,"output_tokens":10,"cached_tokens":"60"}),
        json!({"input_tokens":100,"prompt_tokens":100,"output_tokens":10,"cached_tokens":90,"cache_write_tokens":20}),
    ] {
        assert!(
            usage_from_anthropic(Some(&raw))
                .unwrap()
                .with_estimates(100, 10)
                .is_err(),
            "{raw}"
        );
    }
}

#[test]
fn responses_preserve_aliases_canonical_fallback_and_detect_mirrors() {
    for raw in [
        json!({"input_tokens":100,"output_tokens":10,"cached_tokens":60,"cache_creation_tokens":20}),
        json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":60,"cached_creation_tokens":20}}),
        json!({"input_tokens":null,"prompt_tokens":100,"completion_tokens":10,"cache_read_tokens":60,"cache_write_input_tokens":20}),
    ] {
        let u = usage_from_responses(Some(&raw))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(
            (
                u.prompt_tokens,
                u.prompt_uncached(),
                u.cached_tokens,
                u.cache_write_tokens
            ),
            (100, 20, 60, 20)
        );
    }
    for raw in [
        json!({"input_tokens":100,"prompt_tokens":101,"output_tokens":10}),
        json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":60},"prompt_tokens_details":{"cached_tokens":59}}),
        json!({"input_tokens":100,"output_tokens":10,"cache_write_input_tokens":"20"}),
    ] {
        assert!(
            usage_from_responses(Some(&raw))
                .unwrap()
                .with_estimates(100, 10)
                .is_err()
        );
    }
    assert!(usage_from_responses(Some(&Value::Null)).is_none());
}

#[test]
fn chat_cache_envelopes_survive_json_and_stream_protocol_conversion() {
    use bytes::Bytes;
    use okapi_api::{ChunkProbe, usage_from_chat};
    use okapi_providers::convert::{
        anthropic_to_openai::{OaiStreamToAnthropic, response_openai_to_anthropic},
        gemini_to_openai::{OaiStreamToGemini, response_openai_to_gemini},
    };
    let raw = json!({"id":"c","model":"m","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop","usage":{"cached_tokens":60}}],"usage":{"prompt_tokens":100,"completion_tokens":10}});
    let expected = usage_from_chat(&raw).unwrap().to_token_usage().unwrap();
    let bytes = Bytes::from(raw.to_string());
    for (_, usage) in [
        response_openai_to_anthropic(&bytes).unwrap(),
        response_openai_to_gemini(&bytes).unwrap(),
    ] {
        assert_eq!(usage.unwrap().to_token_usage().unwrap(), expected);
    }
    let chunk: ChunkProbe = serde_json::from_value(raw.clone()).unwrap();
    let event = || {
        Ok(ChatEvent::Data {
            raw: raw.to_string(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage: chunk.usage,
        })
    };
    let mut anthropic = OaiStreamToAnthropic::new("m");
    let mut gemini = OaiStreamToGemini::new("m");
    let mut a = anthropic.step(event());
    a.extend(anthropic.step(Ok(ChatEvent::Done)));
    let mut g = gemini.step(event());
    g.extend(gemini.step(Ok(ChatEvent::Done)));
    for events in [a, g] {
        let usage = events
            .into_iter()
            .filter_map(|e| match e.unwrap() {
                ChatEvent::Data { usage: Some(u), .. } => Some(u),
                _ => None,
            })
            .last()
            .unwrap();
        assert_eq!(usage.to_token_usage().unwrap(), expected);
    }
}
