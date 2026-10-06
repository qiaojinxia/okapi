use okapi_api::UsageProbe;
use serde_json::{Value, json};

fn parse(raw: Value) -> UsageProbe {
    serde_json::from_value(raw).unwrap()
}

#[test]
fn deepseek_top_level_hits_are_reported_and_misses_are_not_writes() {
    for count in [Value::Null, json!(0), json!(60)] {
        let mut raw = json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_cache_hit_tokens":count});
        if let Some(hit) = count.as_u64() {
            raw["prompt_cache_miss_tokens"] = json!(100 - hit);
        }
        let probe = parse(raw);
        let usage = probe.to_token_usage().unwrap();
        assert_eq!(
            usage.cached_tokens,
            u32::try_from(count.as_u64().unwrap_or(0)).unwrap()
        );
        assert_eq!(usage.cache_read_reported, !count.is_null());
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.cache_write_tokens, 0);
        assert!(!usage.cache_write_reported);
        assert_eq!(parse(probe.chat_json()).to_token_usage().unwrap(), usage);
    }
}

#[test]
fn deepseek_mirrors_and_misses_are_validated_without_estimated_billing() {
    let raw = json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_cache_hit_tokens":60,"prompt_cache_miss_tokens":40,
        "prompt_tokens_details":{"cached_tokens":60}});
    assert_eq!(
        parse(raw.clone()).to_token_usage().unwrap().cached_tokens,
        60
    );
    for (pointer, value) in [
        ("/prompt_cache_hit_tokens", json!(0)),
        ("/prompt_cache_hit_tokens", json!(-1)),
        ("/prompt_cache_hit_tokens", json!(1.5)),
        ("/prompt_cache_hit_tokens", json!("60")),
        ("/prompt_cache_miss_tokens", json!(39)),
        ("/prompt_cache_miss_tokens", json!(4_294_967_296_u64)),
        ("/prompt_tokens_details/cached_tokens", json!(61)),
    ] {
        let mut invalid = raw.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(parse(invalid).with_estimates(100, 10).is_err(), "{pointer}");
    }
    // Null does not mask a real observation, and missing hits are not derived
    // from miss tokens even when the arithmetic would be possible.
    let null_mirror = json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_cache_hit_tokens":60,"prompt_tokens_details":{"cached_tokens":null}});
    assert!(
        parse(null_mirror)
            .to_token_usage()
            .unwrap()
            .cache_read_reported
    );
    let miss_only = parse(json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_cache_miss_tokens":40}))
    .to_token_usage()
    .unwrap();
    assert!(!miss_only.cache_read_reported);
    assert!(!miss_only.cache_write_reported);
}

#[test]
fn deepseek_complete_hit_and_miss_observations_preserve_input_in_partial_snapshots() {
    for hit in [0, 60] {
        let raw = json!({"completion_tokens":10,"total_tokens":110,
            "prompt_cache_hit_tokens":hit,"prompt_cache_miss_tokens":100-hit});
        let usage = parse(raw).to_token_usage().unwrap();
        assert_eq!((usage.prompt_tokens, usage.cached_tokens), (100, hit));
        assert!(usage.cache_read_reported);
        assert!(!usage.cache_write_reported);
    }
    let start = parse(json!({"prompt_tokens":100,"completion_tokens":0}));
    let delta = parse(json!({"completion_tokens":10,"prompt_cache_hit_tokens":60,
        "prompt_cache_miss_tokens":39}))
    .with_previous(Some(start));
    assert_eq!(delta.to_token_usage().unwrap().prompt_tokens, 99);
    let overflow = parse(json!({"completion_tokens":10,
        "prompt_cache_hit_tokens":4_294_967_295_u32,"prompt_cache_miss_tokens":1}));
    assert!(overflow.with_estimates(100, 10).is_err());
    let missing_hit = parse(json!({"completion_tokens":10,"prompt_cache_miss_tokens":40}));
    assert!(missing_hit.missing_prompt);
    assert!(!missing_hit.prompt_tokens_details.cache_read_reported);
}

#[test]
fn ark_audio_cache_is_a_subset_and_survives_conversion() {
    let probe = parse(json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_tokens_details":{"cached_tokens":60,"audio_tokens":40,"audio_cached_tokens":20}}));
    let usage = probe.to_token_usage().unwrap();
    assert_eq!(usage.cached_tokens, 60);
    assert_eq!(usage.cache_read_modalities.unwrap().audio_tokens, 20);
    assert_eq!(usage.audio_prompt_tokens, 20);
    assert_eq!(usage.prompt_uncached(), 20);
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(parse(probe.chat_json()).to_token_usage().unwrap(), usage);
    let reported = usage.reported_details.unwrap();
    assert!(reported.cache_read.audio);
    assert!(reported.prompt.audio);
    let zero = parse(json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_tokens_details":{"cached_tokens":40,"audio_tokens":40,"audio_cached_tokens":0}}));
    assert_eq!(zero.to_token_usage().unwrap().audio_prompt_tokens, 40);
    assert!(
        zero.to_token_usage()
            .unwrap()
            .reported_details
            .unwrap()
            .cache_read
            .audio
    );
}

#[test]
fn ark_audio_cache_conflicts_and_invalid_intersections_are_rejected() {
    for details in [
        json!({"cached_tokens":60,"audio_tokens":40,"audio_cached_tokens":20,
            "cached_tokens_details":{"text_tokens":40,"audio_tokens":21}}),
        json!({"cached_tokens":60,"audio_tokens":40,"audio_cached_tokens":41}),
        json!({"cached_tokens":20,"audio_tokens":40,"audio_cached_tokens":21}),
        json!({"cached_tokens":null,"audio_tokens":40,"audio_cached_tokens":20}),
        json!({"cached_tokens":60,"audio_tokens":40,"audio_cached_tokens":-1}),
        json!({"cached_tokens":60,"audio_tokens":40,"audio_cached_tokens":"20"}),
    ] {
        assert!(
            parse(json!({"prompt_tokens":100,"completion_tokens":10,
            "prompt_tokens_details":details}))
            .with_estimates(100, 10)
            .is_err()
        );
    }
}

#[test]
fn stream_cache_snapshots_replace_counts_and_do_not_retain_stale_audio_splits() {
    let start = parse(json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_tokens_details":{"cached_tokens":60,"audio_tokens":40,"audio_cached_tokens":20}}));
    let retained =
        parse(json!({"prompt_tokens":100,"completion_tokens":10})).with_previous(Some(start));
    assert_eq!(retained.to_token_usage().unwrap().cached_tokens, 60);
    let updated = parse(json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_cache_hit_tokens":0,"prompt_tokens_details":{"audio_tokens":40}}))
    .with_previous(Some(retained));
    assert_eq!(updated.to_token_usage().unwrap().cached_tokens, 0);
    assert!(
        updated
            .prompt_tokens_details
            .cached_tokens_details
            .is_none()
    );
    let invalid = parse(json!({"prompt_tokens":100,"completion_tokens":10,
        "prompt_cache_hit_tokens":60,"prompt_tokens_details":{"cached_tokens":0}}))
    .with_previous(Some(updated));
    assert!(
        parse(json!({"prompt_tokens":100,"completion_tokens":10}))
            .with_previous(Some(invalid))
            .to_token_usage()
            .is_err()
    );
}
