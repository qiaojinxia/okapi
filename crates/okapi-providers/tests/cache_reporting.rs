use okapi_api::UsageProbe;
use okapi_providers::ChatEvent;
use okapi_providers::anthropic::{AnthropicEvent, MetaScanner};
use okapi_providers::convert::{openai_to_anthropic, openai_to_gemini};
use okapi_providers::responses::usage_from_responses;
use serde_json::json;

#[test]
fn chat_usage_distinguishes_omitted_null_and_explicit_zero() {
    for details in [
        json!({}),
        json!({"cached_tokens": null, "cache_write_tokens": null}),
    ] {
        let probe: UsageProbe =
            serde_json::from_value(json!({"prompt_tokens": 100, "prompt_tokens_details": details}))
                .unwrap();
        let usage = probe.to_token_usage();
        assert!(!usage.cache_read_reported && !usage.cache_write_reported);
        assert_eq!(usage.cached_tokens, 0);
        assert_eq!(probe.prompt_tokens_details.cache_json(), json!({}));
    }
    let probe: UsageProbe = serde_json::from_value(json!({"prompt_tokens": 100, "prompt_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}})).unwrap();
    assert!(probe.to_token_usage().cache_read_reported);
    assert!(probe.to_token_usage().cache_write_reported);
    assert_eq!(
        probe.prompt_tokens_details.cache_json(),
        json!({"cached_tokens": 0, "cache_write_tokens": 0})
    );
}

#[test]
fn responses_preserves_read_and_write_reporting_without_changing_usage() {
    let missing = usage_from_responses(Some(&json!({"input_tokens": 100})))
        .unwrap()
        .to_token_usage();
    assert!(!missing.cache_read_reported && !missing.cache_write_reported);
    let usage = usage_from_responses(Some(&json!({"input_tokens": 100, "input_tokens_details": {"cached_tokens": 60, "cache_write_tokens": 20}}))).unwrap().to_token_usage();
    assert!(usage.cache_read_reported && usage.cache_write_reported);
    assert_eq!(
        (
            usage.cached_tokens,
            usage.cache_write_tokens,
            usage.prompt_uncached()
        ),
        (60, 20, 20)
    );
}

#[test]
fn native_usage_preserves_provider_specific_reporting() {
    let anthropic = openai_to_anthropic::usage_from_anthropic(Some(&json!({"input_tokens": 20, "cache_read_input_tokens": 60, "cache_creation_input_tokens": 20}))).to_token_usage();
    assert!(anthropic.cache_read_reported && anthropic.cache_write_reported);
    assert_eq!(
        (anthropic.prompt_tokens, anthropic.prompt_uncached()),
        (100, 20)
    );
    let missing = openai_to_anthropic::usage_from_anthropic(Some(&json!({"input_tokens": 20})))
        .to_token_usage();
    assert!(!missing.cache_read_reported && !missing.cache_write_reported);
    let gemini = openai_to_gemini::usage_from_gemini(Some(
        &json!({"promptTokenCount": 100, "cachedContentTokenCount": 0}),
    ))
    .to_token_usage();
    assert!(gemini.cache_read_reported);
    assert!(!gemini.cache_write_reported);
    let missing = openai_to_gemini::usage_from_gemini(Some(&json!({"promptTokenCount": 100})))
        .to_token_usage();
    assert!(!missing.cache_read_reported);
}

#[test]
fn anthropic_streams_keep_reporting_flags_from_message_start() {
    for reported in [false, true] {
        let mut raw = json!({"input_tokens": 20});
        if reported {
            raw["cache_read_input_tokens"] = json!(60);
            raw["cache_creation_input_tokens"] = json!(20);
        }
        let start = || AnthropicEvent {
            event: "message_start".into(),
            data: json!({"message": {"usage": raw}}).to_string(),
        };
        let end = || AnthropicEvent {
            event: "message_delta".into(),
            data: json!({"usage": {"output_tokens": 10}}).to_string(),
        };
        let mut native = MetaScanner::new();
        native.scan(Ok(start()));
        let mut converted = openai_to_anthropic::StreamState::new("fixture");
        converted.step(Ok(start()));
        for events in [native.scan(Ok(end())), converted.step(Ok(end()))] {
            let usage = events
                .into_iter()
                .find_map(|event| match event.unwrap() {
                    ChatEvent::Data { usage, .. } => usage,
                    ChatEvent::Done => None,
                })
                .unwrap()
                .to_token_usage();
            assert_eq!(usage.cache_read_reported, reported);
            assert_eq!(usage.cache_write_reported, reported);
            assert_eq!(usage.prompt_tokens, if reported { 100 } else { 20 });
        }
    }
}
