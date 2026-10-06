use okapi_api::{ChunkProbe, UsageProbe, usage_from_chat};
use serde_json::{Value, json};

fn probe(value: Value) -> UsageProbe {
    serde_json::from_value(value).unwrap()
}

#[test]
fn aliases_at_both_levels_preserve_observations_and_bill_each_subset_once() {
    for read in [
        "cached_tokens",
        "cache_read_tokens",
        "cache_read_input_tokens",
        "prompt_cache_hit_tokens",
    ] {
        for write in [
            "cache_write_tokens",
            "cache_creation_tokens",
            "cache_write_input_tokens",
            "cached_creation_tokens",
            "cache_creation_input_tokens",
            "created_cache_tokens",
        ] {
            for nested in [false, true] {
                let mut raw = json!({"prompt_tokens":100,"completion_tokens":10});
                let target = if nested {
                    raw["prompt_tokens_details"] = json!({});
                    &mut raw["prompt_tokens_details"]
                } else {
                    &mut raw
                };
                target[read] = json!(60);
                target[write] = json!(20);
                let parsed = probe(raw);
                let usage = parsed.to_token_usage().unwrap();
                assert_eq!(
                    (
                        usage.prompt_uncached(),
                        usage.cached_tokens,
                        usage.cache_write_tokens
                    ),
                    (20, 60, 20),
                    "{read} {write} nested={nested}"
                );
                assert!(usage.cache_read_reported && usage.cache_write_reported);
                assert_eq!(probe(parsed.chat_json()).to_token_usage().unwrap(), usage);
            }
        }
    }
}

#[test]
fn mirror_conflicts_malformed_values_and_partial_lifetimes_are_invalid() {
    for extra in [
        json!({"cached_tokens":59,"prompt_tokens_details":{"cached_tokens":60}}),
        json!({"cache_write_tokens":20,"input_tokens_details":{"cache_creation_tokens":21}}),
        json!({"cache_write_tokens":20,"prompt_tokens_details":{"cache_write_tokens":0}}),
        json!({"cache_write_tokens":20,"claude_cache_creation_5_m_tokens":20}),
        json!({"cache_write_tokens":20,"claude_cache_creation_5_m_tokens":12,"claude_cache_creation_1_h_tokens":9}),
        json!({"input_tokens_details":[]}),
    ] {
        let mut raw = extra;
        raw["prompt_tokens"] = json!(100);
        raw["completion_tokens"] = json!(10);
        assert!(probe(raw).with_estimates(100, 10).is_err());
    }
    for field in [
        "cache_read_tokens",
        "cached_tokens",
        "cache_creation_tokens",
        "cached_creation_tokens",
        "cache_write_input_tokens",
    ] {
        for invalid in [
            json!(-1),
            json!(1.5),
            json!("20"),
            json!(4_294_967_296_u64),
            json!(101),
        ] {
            let mut raw = json!({"prompt_tokens":100,"completion_tokens":10});
            raw[field] = invalid;
            assert!(probe(raw).with_estimates(100, 10).is_err(), "{field}");
        }
    }
}

#[test]
fn legacy_lifetimes_and_missing_zero_counters_remain_distinct() {
    let usage = probe(json!({"prompt_tokens":100,"completion_tokens":10,"cached_creation_tokens":20,"claude_cache_creation_5_m_tokens":12,"claude_cache_creation_1_h_tokens":8})).to_token_usage().unwrap();
    assert_eq!(
        (usage.cache_write_5m_tokens, usage.cache_write_1h_tokens),
        (Some(12), Some(8))
    );
    for value in [Value::Null, json!(0), json!(20)] {
        let usage = probe(json!({"prompt_tokens":100,"completion_tokens":10,"cached_tokens":value,"cache_creation_tokens":value})).to_token_usage().unwrap();
        assert_eq!(
            (usage.cache_read_reported, usage.cache_write_reported),
            (!value.is_null(), !value.is_null())
        );
    }
    let usage = probe(json!({"prompt_tokens":100,"completion_tokens":10,"claude_cache_creation_5_m_tokens":0,"claude_cache_creation_1_h_tokens":0})).to_token_usage().unwrap();
    assert!(!usage.cache_write_reported);
    assert_eq!(usage.cache_write_5m_tokens, None);
}

#[test]
fn kimi_and_llamacpp_envelopes_share_json_and_chunk_parsing() {
    for fields in [
        json!({"choices":[{"usage":{"cached_tokens":60}},{"usage":{"cached_tokens":60}}]}),
        json!({"timings":{"cache_n":60}}),
    ] {
        let mut raw = fields;
        raw["usage"] = json!({"prompt_tokens":100,"completion_tokens":10});
        let json = usage_from_chat(&raw).unwrap().to_token_usage().unwrap();
        let chunk: ChunkProbe = serde_json::from_value(raw).unwrap();
        assert_eq!(chunk.usage.unwrap().to_token_usage().unwrap(), json);
        assert_eq!(json.cached_tokens, 60);
        assert_eq!(json.prompt_uncached(), 40);
        assert!(!json.cache_write_reported);
    }
    for raw in [
        json!({"usage":{"prompt_tokens":100,"completion_tokens":10,"cached_tokens":0},"timings":{"cache_n":60}}),
        json!({"choices":[{"usage":{"cached_tokens":60}},{"usage":{"cached_tokens":61}}]}),
        json!({"choices":[{"usage":{"cached_tokens":"60"}}]}),
        json!({"timings":{"cache_n":-1}}),
    ] {
        assert!(
            usage_from_chat(&raw)
                .unwrap()
                .with_estimates(100, 10)
                .is_err()
        );
    }
}

#[test]
fn cache_only_chunks_retain_axes_but_explicit_zero_replaces_cache() {
    let first = usage_from_chat(&json!({"usage":{"prompt_tokens":100,"completion_tokens":10},"choices":[{"usage":{"cached_tokens":60}}]})).unwrap();
    let next = usage_from_chat(&json!({"timings":{"cache_n":0}})).unwrap();
    assert!(next.missing_prompt && next.missing_completion);
    let retained = next.with_previous(Some(first)).to_token_usage().unwrap();
    assert_eq!(
        (
            retained.prompt_tokens,
            retained.completion_tokens,
            retained.cached_tokens
        ),
        (100, 10, 0)
    );
    assert!(retained.cache_read_reported);
    let missing = usage_from_chat(&json!({"choices":[{"usage":{"cached_tokens":null}}]}));
    assert!(missing.is_none());
    assert!(probe(json!({})).with_estimates(100, 10).is_err());
}
