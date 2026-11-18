//! Native usage shapes retained in an OpenAI-compatible bridge's usage object.
use okapi_api::UsageProbe;
use serde_json::{Value, json};

fn parse(raw: Value) -> UsageProbe {
    serde_json::from_value(raw).unwrap()
}

#[test]
fn bedrock_exclusive_input_and_cache_ttl_are_normalized_once() {
    for total in [30, 110] {
        let probe = parse(
            json!({"inputTokens":20,"outputTokens":10,"totalTokens":total,
            "cacheReadInputTokens":60,"cacheWriteInputTokens":20,
            "cacheDetails":[{"ttl":"1h","inputTokens":8},{"ttl":"5m","inputTokens":12}]}),
        );
        let usage = probe.to_token_usage().unwrap();
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (100, 10));
        assert_eq!(usage.prompt_uncached(), 20);
        assert_eq!(usage.cached_tokens, 60);
        assert_eq!(usage.cache_write_tokens, 20);
        assert_eq!(
            (usage.cache_write_5m_tokens, usage.cache_write_1h_tokens),
            (Some(12), Some(8))
        );
        assert!(usage.cache_read_reported && usage.cache_write_reported);
        assert_eq!(parse(probe.chat_json()).to_token_usage().unwrap(), usage);
    }
}

#[test]
fn bedrock_null_missing_and_explicit_zero_keep_their_observation_state() {
    for (fields, reported, ttl) in [
        (json!({}), false, (None, None)),
        (
            json!({"cacheReadInputTokens":null,"cacheWriteInputTokens":null,"cacheDetails":null}),
            false,
            (None, None),
        ),
        (json!({"cacheDetails":[]}), false, (None, None)),
        (
            json!({"cacheReadInputTokens":0,"cacheWriteInputTokens":0}),
            true,
            (None, None),
        ),
        (
            json!({"cacheReadInputTokens":0,"cacheWriteInputTokens":0,"cacheDetails":[]}),
            true,
            (Some(0), Some(0)),
        ),
    ] {
        let mut raw = json!({"inputTokens":100,"outputTokens":10,"totalTokens":110});
        raw.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let usage = parse(raw).to_token_usage().unwrap();
        assert_eq!(
            (usage.cache_read_reported, usage.cache_write_reported),
            (reported, reported)
        );
        assert_eq!(
            (usage.cache_write_5m_tokens, usage.cache_write_1h_tokens),
            ttl
        );
        assert_eq!((usage.cached_tokens, usage.cache_write_tokens), (0, 0));
    }
}

#[test]
fn null_bridge_axes_do_not_hide_observed_cache_fields() {
    for raw in [
        json!({"prompt_tokens":100,"completion_tokens":10,"inputTokens":null,
            "total_input_tokens":null,"cacheReadInputTokens":60}),
        json!({"inputTokens":40,"outputTokens":10,"cacheReadInputTokens":60,
            "total_cached_tokens":null}),
        json!({"total_input_tokens":100,"total_output_tokens":10,
            "total_cached_tokens":60,"cacheReadInputTokens":null}),
    ] {
        let usage = parse(raw).to_token_usage().unwrap();
        assert_eq!((usage.prompt_tokens, usage.cached_tokens), (100, 60));
        assert!(usage.cache_read_reported);
        assert!(!usage.cache_write_reported);
    }
    let usage = parse(json!({"prompt_tokens":100,"completion_tokens":10,
        "cacheReadInputTokens":null,"total_cached_tokens":null}))
    .to_token_usage()
    .unwrap();
    assert!(!usage.cache_read_reported && !usage.cache_write_reported);
}

#[test]
fn bedrock_invalid_known_fields_and_conflicting_lifetimes_are_not_estimated() {
    let raw = json!({"inputTokens":20,"outputTokens":10,"totalTokens":30,
        "cacheReadInputTokens":60,"cacheWriteInputTokens":20});
    for (key, value) in [
        ("inputTokens", json!(-1)),
        ("outputTokens", json!(1.5)),
        ("cacheReadInputTokens", json!("60")),
        ("cacheWriteInputTokens", json!(4_294_967_296_u64)),
        ("inputTokens", json!(4_294_967_295_u32)),
        ("totalTokens", json!(31)),
        ("cacheDetails", json!([])),
        ("cacheDetails", json!([{"ttl":"5m","inputTokens":19}])),
        (
            "cacheDetails",
            json!([{"ttl":"5m","inputTokens":10},{"ttl":"5m","inputTokens":10}]),
        ),
        ("cacheDetails", json!([{"ttl":"30m","inputTokens":20}])),
        ("cacheDetails", json!([{"ttl":"1h","inputTokens":-1}])),
        ("cacheDetails", json!({"ttl":"5m","inputTokens":20})),
        ("prompt_tokens", json!(100)),
        ("total_input_tokens", json!(100)),
    ] {
        let mut invalid = raw.clone();
        invalid[key] = value;
        assert!(parse(invalid).with_estimates(100, 10).is_err(), "{key}");
    }
}

fn interactions() -> Value {
    json!({"total_input_tokens":100,"total_output_tokens":10,"total_thought_tokens":5,
        "total_tokens":115,"total_cached_tokens":60,
        "input_tokens_by_modality":[{"modality":"text","tokens":60},{"modality":"audio","tokens":40}],
        "output_tokens_by_modality":[{"modality":"text","tokens":10}],
        "cached_tokens_by_modality":[{"modality":"text","tokens":40},{"modality":"audio","tokens":20}]})
}

#[test]
fn interactions_cached_modalities_and_thoughts_are_included_in_the_right_axes() {
    let probe = parse(interactions());
    let usage = probe.to_token_usage().unwrap();
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.reasoning_tokens
        ),
        (100, 15, 5)
    );
    assert_eq!(usage.cached_tokens, 60);
    assert_eq!(usage.cache_read_modalities.unwrap().audio_tokens, 20);
    assert_eq!(
        (usage.audio_prompt_tokens, usage.prompt_uncached()),
        (20, 20)
    );
    assert!(usage.cache_read_reported);
    assert!(!usage.cache_write_reported);
    assert_eq!(parse(probe.chat_json()).to_token_usage().unwrap(), usage);
    // Official function-call response: 100 input + 25 output = 125 total;
    // total_tool_use_tokens=50 does not add another 50 input tokens.
    let usage = parse(json!({"total_input_tokens":100,"total_output_tokens":25,
        "total_thought_tokens":0,"total_tool_use_tokens":50,"total_tokens":125,
        "total_cached_tokens":0}))
    .to_token_usage()
    .unwrap();
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (100, 25));
}

#[test]
fn interactions_cache_missing_null_zero_and_text_only_details_are_distinct() {
    for cached in [Value::Null, json!(0), json!(60)] {
        let mut raw = json!({"total_input_tokens":100,"total_output_tokens":10,"total_tokens":110});
        raw["total_cached_tokens"] = cached.clone();
        let usage = parse(raw).to_token_usage().unwrap();
        assert_eq!(usage.cache_read_reported, !cached.is_null());
        assert_eq!(
            usage.cached_tokens,
            u32::try_from(cached.as_u64().unwrap_or(0)).unwrap()
        );
        assert!(!usage.cache_write_reported);
    }
    let usage = parse(json!({"total_input_tokens":100,"total_output_tokens":10,
        "total_cached_tokens":60,"cached_tokens_by_modality":[{"modality":"text","tokens":60}]}))
    .to_token_usage()
    .unwrap();
    assert_eq!(usage.prompt_uncached(), 40);
    assert_eq!(usage.cache_read_modalities.unwrap().audio_tokens, 0);
}

#[test]
fn interactions_malformed_and_ambiguous_modal_usage_never_falls_back_to_estimates() {
    for (key, value) in [
        ("total_cached_tokens", json!(101)),
        ("total_cached_tokens", json!(-1)),
        ("total_cached_tokens", json!("60")),
        ("total_tokens", json!(110)),
        ("total_thought_tokens", json!(4_294_967_295_u32)),
        ("total_tool_use_tokens", json!(-1)),
        (
            "input_tokens_by_modality",
            json!([{"modality":"audio","tokens":101}]),
        ),
        (
            "cached_tokens_by_modality",
            json!([{"modality":"audio","tokens":20}]),
        ),
        (
            "cached_tokens_by_modality",
            json!([{"modality":"audio","tokens":41},{"modality":"text","tokens":19}]),
        ),
        (
            "cached_tokens_by_modality",
            json!([{"modality":"audio","tokens":30},{"modality":"audio","tokens":30}]),
        ),
        (
            "cached_tokens_by_modality",
            json!([{"modality":"unknown","tokens":60}]),
        ),
        ("cached_tokens_by_modality", Value::Null),
        ("prompt_tokens", json!(100)),
        ("inputTokens", json!(20)),
    ] {
        let mut invalid = interactions();
        invalid[key] = value;
        assert!(parse(invalid).with_estimates(100, 10).is_err(), "{key}");
    }
}

#[test]
fn bridged_streams_keep_missing_snapshots_and_replace_explicit_zero() {
    for initial in [
        json!({"inputTokens":20,"outputTokens":10,"cacheReadInputTokens":60,"cacheWriteInputTokens":20,
            "cacheDetails":[{"ttl":"5m","inputTokens":20}]}),
        json!({"total_input_tokens":100,"total_output_tokens":10,"total_cached_tokens":60}),
    ] {
        let start = parse(initial);
        let retained = parse(json!({"completion_tokens":10})).with_previous(Some(start));
        assert_eq!(retained.to_token_usage().unwrap().cached_tokens, 60);
        let updated = parse(json!({"prompt_tokens":100,"completion_tokens":10,
            "prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":0}}))
        .with_previous(Some(retained));
        let usage = updated.to_token_usage().unwrap();
        assert_eq!((usage.cached_tokens, usage.cache_write_tokens), (0, 0));
        assert!(usage.cache_read_reported && usage.cache_write_reported);
        assert_eq!(
            (usage.cache_write_5m_tokens, usage.cache_write_1h_tokens),
            (None, None)
        );
    }
}

#[test]
fn canonical_bridges_can_keep_native_cache_fields_without_adding_them_to_input_again() {
    for extras in [
        json!({"cacheReadInputTokens":60,"cacheWriteInputTokens":20,
            "cacheDetails":[{"ttl":"5m","inputTokens":20}]}),
        json!({"total_cached_tokens":60}),
    ] {
        let mut raw = json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110});
        raw.as_object_mut()
            .unwrap()
            .extend(extras.as_object().unwrap().clone());
        let usage = parse(raw.clone()).to_token_usage().unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.cached_tokens, 60);
        assert!(usage.cache_read_reported);
        raw["prompt_tokens_details"] = json!({"cached_tokens":60});
        assert_eq!(parse(raw.clone()).to_token_usage().unwrap(), usage);
        raw["prompt_tokens_details"]["cached_tokens"] = json!(59);
        assert!(parse(raw).with_estimates(100, 10).is_err());
    }
    let mut raw = interactions();
    raw.as_object_mut().unwrap().remove("total_input_tokens");
    raw.as_object_mut().unwrap().remove("total_output_tokens");
    raw.as_object_mut().unwrap().remove("total_thought_tokens");
    raw.as_object_mut()
        .unwrap()
        .remove("input_tokens_by_modality");
    raw.as_object_mut()
        .unwrap()
        .remove("output_tokens_by_modality");
    raw["prompt_tokens"] = json!(100);
    raw["completion_tokens"] = json!(15);
    raw["prompt_tokens_details"] = json!({"audio_tokens":40});
    let usage = parse(raw.clone()).to_token_usage().unwrap();
    assert_eq!(usage.cache_read_modalities.unwrap().audio_tokens, 20);
    assert_eq!(usage.audio_prompt_tokens, 20);
    raw["prompt_tokens_details"]["cached_tokens_details"] = json!({"audio_tokens":21});
    assert!(parse(raw).with_estimates(100, 15).is_err());
}
