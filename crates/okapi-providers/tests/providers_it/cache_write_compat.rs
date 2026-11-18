use bytes::Bytes;
use okapi_api::UsageProbe;
use okapi_providers::ChatEvent;
use okapi_providers::convert::anthropic_to_openai::{
    OaiStreamToAnthropic, response_openai_to_anthropic,
};
use okapi_providers::convert::openai_to_anthropic::usage_from_anthropic;
use okapi_providers::responses::usage_from_responses;
use serde_json::{Value, json};

#[test]
fn responses_normalizes_compatible_write_fields_and_reported_zero() {
    for field in ["cache_creation_input_tokens", "created_cache_tokens"] {
        for value in [Value::Null, json!(0), json!(20)] {
            let mut details = json!({"cached_tokens":60});
            details[field] = value.clone();
            let usage = usage_from_responses(Some(&json!({
                "input_tokens":100,"output_tokens":10,"total_tokens":110,
                "input_tokens_details":details,
            })))
            .unwrap()
            .to_token_usage()
            .unwrap();
            let written = u32::try_from(value.as_u64().unwrap_or(0)).unwrap();
            assert_eq!(usage.prompt_tokens, 100);
            assert_eq!(usage.prompt_uncached(), 40 - written);
            assert_eq!(usage.cache_write_tokens, written);
            assert_eq!(usage.cache_write_reported, !value.is_null());
        }
    }
}

#[test]
fn compatible_writes_survive_json_and_sse_conversion_to_anthropic() {
    for details in [
        json!({"cached_tokens":60,"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_5m_input_tokens":20}}),
        json!({"cached_tokens":60,"created_cache_tokens":20}),
    ] {
        let raw = json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_tokens_details":details});
        let probe: UsageProbe = serde_json::from_value(raw.clone()).unwrap();
        let expected = probe.to_token_usage().unwrap();
        let response = json!({"model":"fixture","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":raw});
        let (bytes, converted) =
            response_openai_to_anthropic(&Bytes::from(response.to_string())).unwrap();
        assert_eq!(converted.unwrap().to_token_usage().unwrap(), expected);
        let native: Value = serde_json::from_slice(&bytes).unwrap();
        assert_native_usage(&native["usage"], probe);

        let mut stream = OaiStreamToAnthropic::new("fixture");
        stream.step(Ok(ChatEvent::Data {
            raw: json!({"choices":[],"usage":raw}).to_string(),
            event: None,
            has_output: false,
            content_chars: 0,
            usage: Some(probe),
        }));
        let terminal = stream.step(Ok(ChatEvent::Done));
        let mut found = false;
        for event in terminal {
            if let ChatEvent::Data {
                raw,
                event: Some(name),
                usage,
                ..
            } = event.unwrap()
                && name == "message_delta"
            {
                found = true;
                assert_eq!(usage.unwrap().to_token_usage().unwrap(), expected);
                assert_native_usage(
                    &serde_json::from_str::<Value>(&raw).unwrap()["usage"],
                    probe,
                );
            }
        }
        assert!(found, "missing terminal usage");
    }
}

fn assert_native_usage(native: &Value, expected: UsageProbe) {
    assert_eq!(native["input_tokens"], 20);
    assert_eq!(native["cache_creation_input_tokens"], 20);
    assert_eq!(native["cache_read_input_tokens"], 60);
    assert_eq!(
        usage_from_anthropic(Some(native))
            .unwrap()
            .to_token_usage()
            .unwrap(),
        expected.to_token_usage().unwrap()
    );
}
