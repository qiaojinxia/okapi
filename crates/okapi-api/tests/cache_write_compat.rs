use okapi_api::UsageProbe;
use serde_json::{Value, json};

fn parse(details: Value) -> UsageProbe {
    let mut raw = json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110});
    raw["prompt_tokens_details"] = details;
    serde_json::from_value(raw).unwrap()
}

#[test]
fn compatible_write_fields_preserve_missing_zero_and_counts_without_double_counting() {
    for field in [
        "cache_write_tokens",
        "cache_creation_input_tokens",
        "created_cache_tokens",
    ] {
        for value in [Value::Null, json!(0), json!(20)] {
            let mut details = json!({"cached_tokens":60});
            details[field] = value.clone();
            let probe = parse(details);
            let usage = probe.to_token_usage().unwrap();
            let written = u32::try_from(value.as_u64().unwrap_or(0)).unwrap();
            assert_eq!(usage.prompt_tokens, 100);
            assert_eq!(usage.cache_write_tokens, written, "{field}: {value}");
            assert_eq!(usage.cache_write_reported, !value.is_null());
            assert_eq!(usage.prompt_uncached(), 40 - written);
            let canonical = probe.chat_json();
            assert_eq!(
                canonical["prompt_tokens_details"]["cache_write_tokens"],
                value
            );
            assert_eq!(
                serde_json::from_value::<UsageProbe>(canonical)
                    .unwrap()
                    .to_token_usage()
                    .unwrap(),
                usage
            );
        }
    }
}

#[test]
fn duplicate_write_fields_must_agree_and_null_does_not_hide_a_reported_value() {
    for details in [
        json!({"cache_write_tokens":20,"cache_creation_input_tokens":20,"created_cache_tokens":20}),
        json!({"cache_write_tokens":null,"cache_creation_input_tokens":20}),
        json!({"cache_creation_input_tokens":20,"created_cache_tokens":null}),
    ] {
        let usage = parse(details).to_token_usage().unwrap();
        assert_eq!(usage.cache_write_tokens, 20);
        assert!(usage.cache_write_reported);
    }
    for details in [
        json!({"cache_write_tokens":0,"cache_creation_input_tokens":20}),
        json!({"cache_creation_input_tokens":20,"created_cache_tokens":21}),
        json!({"cache_write_tokens":20,"created_cache_tokens":0}),
    ] {
        assert!(parse(details).with_estimates(100, 10).is_err());
    }
}

#[test]
fn qwen_cache_creation_retains_reported_lifetimes_and_does_not_invent_them() {
    for (details, expected) in [
        (
            json!({"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_5m_input_tokens":20}}),
            (Some(20), Some(0)),
        ),
        (
            json!({"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_5m_input_tokens":12,"ephemeral_1h_input_tokens":8}}),
            (Some(12), Some(8)),
        ),
        (
            json!({"cache_creation_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":0}}),
            (Some(0), Some(0)),
        ),
        (json!({"cache_creation_input_tokens":20}), (None, None)),
        (
            json!({"cache_creation_input_tokens":20,"cache_creation":{}}),
            (None, None),
        ),
    ] {
        let probe = parse(details);
        let usage = probe.to_token_usage().unwrap();
        assert_eq!(
            (usage.cache_write_5m_tokens, usage.cache_write_1h_tokens),
            expected
        );
        assert_eq!(
            parse(probe.prompt_tokens_details.cache_json())
                .to_token_usage()
                .unwrap(),
            usage
        );
    }
}

#[test]
fn malformed_write_counters_and_lifetime_conflicts_cannot_fall_back_to_estimates() {
    for field in ["cache_creation_input_tokens", "created_cache_tokens"] {
        for value in [
            json!(-1),
            json!(1.5),
            json!("20"),
            json!(4_294_967_296_u64),
            json!(101),
        ] {
            let mut details = json!({});
            details[field] = value;
            assert!(parse(details).with_estimates(100, 10).is_err(), "{field}");
        }
    }
    for details in [
        json!({"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_5m_input_tokens":19}}),
        json!({"cache_creation":{"ephemeral_5m_input_tokens":0}}),
        json!({"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_5m_input_tokens":-1}}),
        json!({"cache_creation_input_tokens":20,"cache_creation":[20,0]}),
        json!({"cache_creation_input_tokens":20,"cache_creation":"20"}),
        json!({"cache_write_tokens":20,"cache_write_5m_tokens":20,"cache_write_1h_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":19,"ephemeral_1h_input_tokens":1}}),
    ] {
        assert!(parse(details).with_estimates(100, 10).is_err());
    }
}

#[test]
fn streaming_writes_are_snapshots_and_updated_totals_discard_stale_lifetimes() {
    let start = parse(
        json!({"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_5m_input_tokens":20}}),
    );
    let retained = parse(json!({})).with_previous(Some(start));
    let usage = retained.to_token_usage().unwrap();
    assert_eq!(
        (usage.cache_write_tokens, usage.cache_write_5m_tokens),
        (20, Some(20))
    );
    let updated = parse(json!({"created_cache_tokens":30})).with_previous(Some(retained));
    let usage = updated.to_token_usage().unwrap();
    assert_eq!(
        (
            usage.cache_write_tokens,
            usage.cache_write_5m_tokens,
            usage.cache_write_1h_tokens
        ),
        (30, None, None)
    );
    let zero = parse(json!({"cache_creation_input_tokens":0})).with_previous(Some(updated));
    assert_eq!(zero.to_token_usage().unwrap().cache_write_tokens, 0);
    assert!(zero.to_token_usage().unwrap().cache_write_reported);
    let invalid =
        parse(json!({"cache_write_tokens":0,"created_cache_tokens":20})).with_previous(Some(zero));
    assert!(
        parse(json!({"created_cache_tokens":20}))
            .with_previous(Some(invalid))
            .to_token_usage()
            .is_err()
    );
}
