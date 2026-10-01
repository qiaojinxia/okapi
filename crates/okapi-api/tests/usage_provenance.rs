use okapi_api::UsageProbe;
use okapi_domain::{TokenUsage, UpstreamTokenCounts};
use serde_json::{Value, json};

fn parse(value: Value) -> UsageProbe {
    serde_json::from_value(value).unwrap()
}

#[test]
fn cache_write_ttl_extensions_preserve_zero_and_reject_partial_or_invalid_splits() {
    let body = json!({"prompt_tokens":100,"completion_tokens":10,"prompt_tokens_details":{
        "cache_write_tokens":40,"cache_write_5m_tokens":40,"cache_write_1h_tokens":0}});
    let probe = parse(body.clone());
    let usage = probe.to_token_usage().unwrap();
    assert_eq!(
        (usage.cache_write_5m_tokens, usage.cache_write_1h_tokens),
        (Some(40), Some(0))
    );
    assert_eq!(parse(probe.chat_json()).to_token_usage().unwrap(), usage);
    for invalid in [
        json!({"cache_write_tokens":40,"cache_write_5m_tokens":40}),
        json!({"cache_write_tokens":40,"cache_write_5m_tokens":40,"cache_write_1h_tokens":1}),
        json!({"cache_write_5m_tokens":0,"cache_write_1h_tokens":0}),
    ] {
        let mut value = body.clone();
        value["prompt_tokens_details"] = invalid;
        assert!(parse(value).to_token_usage().is_err());
    }
    let mut legacy = body;
    legacy["prompt_tokens_details"] = json!({"cache_write_tokens":40});
    let legacy = parse(legacy).to_token_usage().unwrap();
    assert_eq!(
        (legacy.cache_write_5m_tokens, legacy.cache_write_1h_tokens),
        (None, None)
    );
}

#[test]
fn missing_axes_are_estimated_while_explicit_zero_and_original_counts_survive() {
    for (raw, expected, sources) in [
        (
            json!({"prompt_tokens":0}),
            (0, 23),
            ("upstream", "estimated"),
        ),
        (
            json!({"completion_tokens":0}),
            (17, 0),
            ("estimated", "upstream"),
        ),
        (
            json!({"prompt_tokens":101,"completion_tokens":55}),
            (101, 55),
            ("upstream", "upstream"),
        ),
    ] {
        let probe = parse(raw.clone());
        let usage = probe.with_estimates(17, 23).unwrap();
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), expected);
        assert_eq!((usage.prompt_source(), usage.completion_source()), sources);
        let upstream = usage.upstream_usage.unwrap();
        assert_eq!(json!(upstream.prompt_tokens), raw["prompt_tokens"]);
        assert_eq!(json!(upstream.completion_tokens), raw["completion_tokens"]);
        // A second hop must not turn estimated axes into explicitly reported zero.
        let reparse = parse(probe.chat_json()).with_estimates(17, 23).unwrap();
        assert_eq!(reparse, usage);
    }
}

#[test]
fn provider_total_can_recover_one_axis_but_conflicts_never_estimate() {
    for raw in [
        json!({"prompt_tokens":12,"total_tokens":21}),
        json!({"completion_tokens":9,"total_tokens":21}),
    ] {
        let usage = parse(raw).to_token_usage().unwrap();
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (12, 9));
        assert_eq!(
            (usage.prompt_source(), usage.completion_source()),
            ("upstream", "upstream")
        );
    }
    for raw in [
        json!({}),
        json!({"total_tokens":21}),
        json!({"prompt_tokens":12,"total_tokens":11}),
        json!({"prompt_tokens":12,"completion_tokens":9,"total_tokens":22}),
        json!({"prompt_tokens":-1}),
        json!({"completion_tokens":1.5}),
        json!({"prompt_tokens":"1"}),
        json!({"completion_tokens":2_147_483_648_u64}),
    ] {
        assert!(parse(raw.clone()).with_estimates(17, 23).is_err(), "{raw}");
    }
}

#[test]
fn estimates_preserve_known_subsets_and_legacy_provenance_is_unknown() {
    let usage = parse(json!({"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":100,"cache_write_tokens":20}})).with_estimates(17, 23).unwrap();
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.cached_tokens,
            usage.cache_write_tokens
        ),
        (120, 100, 20)
    );
    assert_eq!(usage.prompt_source(), "estimated");
    let mut value = TokenUsage::default();
    assert_eq!(value.prompt_source(), "unknown");
    assert!(
        serde_json::to_value(value)
            .unwrap()
            .get("upstream_usage")
            .is_none()
    );
    value.upstream_usage = Some(UpstreamTokenCounts {
        prompt_tokens: Some(0),
        completion_tokens: None,
    });
    value.prompt_tokens = 20;
    assert_eq!(value.prompt_source(), "local_override");
    assert_eq!(value.completion_source(), "estimated");
    assert_eq!(
        serde_json::from_value::<TokenUsage>(serde_json::to_value(value).unwrap()).unwrap(),
        value
    );
}

#[test]
fn cumulative_snapshots_merge_missing_axes_and_invalid_data_stays_invalid() {
    let input = parse(json!({"prompt_tokens":100,"prompt_tokens_details":{"cached_tokens":50}}));
    let output = parse(json!({"completion_tokens":25}));
    for merged in [
        output.with_previous(Some(input)),
        input.with_previous(Some(output)),
    ] {
        let usage = merged.to_token_usage().unwrap();
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.cached_tokens
            ),
            (100, 25, 50)
        );
        assert_eq!(
            merged.with_previous(Some(merged)).to_token_usage().unwrap(),
            usage
        );
    }
    for invalid in [
        json!({"completion_tokens":-1}),
        json!({"prompt_tokens":1,"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":2}}),
    ] {
        let merged = output.with_previous(Some(parse(invalid)));
        assert!(merged.with_estimates(0, 0).is_err());
    }
}

#[test]
fn missing_detail_fields_and_explicit_zero_remain_distinct_after_json_round_trip() {
    let absent = parse(json!({"prompt_tokens":100,"completion_tokens":50}));
    let zero = parse(json!({"prompt_tokens":100,"completion_tokens":50,
        "prompt_tokens_details":{"audio_tokens":0,"image_tokens":0,"cached_tokens":0,"cache_write_tokens":0},
        "completion_tokens_details":{"audio_tokens":0,"image_tokens":0,"reasoning_tokens":0}}));
    for probe in [absent, zero] {
        let usage = probe.to_token_usage().unwrap();
        let round_trip = parse(probe.chat_json()).to_token_usage().unwrap();
        assert_eq!(round_trip, usage);
        assert_eq!(usage.total_raw(), 150);
    }
    let unknown = absent.to_token_usage().unwrap().reported_details.unwrap();
    assert_eq!(unknown, okapi_domain::TokenDetailsReported::default());
    let known = zero.to_token_usage().unwrap().reported_details.unwrap();
    assert!(known.prompt.audio && known.prompt.image);
    assert!(known.completion.audio && known.completion.image && known.reasoning);
    assert!(known.cache_read.audio && known.cache_read.image);
    assert!(known.cache_write.audio && known.cache_write.image);
    assert!(
        absent.chat_json()["completion_tokens_details"]
            .get("reasoning_tokens")
            .is_none()
    );
    assert_eq!(
        zero.chat_json()["completion_tokens_details"]["reasoning_tokens"],
        0
    );
    let legacy: TokenUsage = serde_json::from_value(
        json!({"prompt_tokens":100,"cached_tokens":0,"completion_tokens":50,"reasoning_tokens":0}),
    )
    .unwrap();
    assert!(legacy.reported_details.is_none());
}

#[test]
fn cumulative_totals_do_not_erase_details_but_explicit_zero_replaces_them() {
    let previous = parse(json!({"prompt_tokens":100,"completion_tokens":50,
        "prompt_tokens_details":{"audio_tokens":10,"image_tokens":5,"cached_tokens":20,
            "cached_tokens_details":{"text_tokens":20,"audio_tokens":0,"image_tokens":0},
            "cache_write_tokens":10,"cache_write_5m_tokens":10,"cache_write_1h_tokens":0,
            "cache_write_tokens_details":{"text_tokens":10,"audio_tokens":0,"image_tokens":0}},
        "completion_tokens_details":{"reasoning_tokens":8,"audio_tokens":9,"image_tokens":7}}));
    let current =
        parse(json!({"prompt_tokens":120,"completion_tokens":60})).with_previous(Some(previous));
    let u = current.to_token_usage().unwrap();
    assert_eq!(
        (
            u.prompt_tokens,
            u.completion_tokens,
            u.cached_tokens,
            u.cache_write_tokens
        ),
        (120, 60, 20, 10)
    );
    assert_eq!(
        (
            u.audio_prompt_tokens,
            u.image_prompt_tokens,
            u.reasoning_tokens,
            u.audio_completion_tokens,
            u.image_completion_tokens
        ),
        (10, 5, 8, 9, 7)
    );
    assert_eq!(
        (u.cache_write_5m_tokens, u.cache_write_1h_tokens),
        (Some(10), Some(0))
    );
    let zero = parse(json!({"prompt_tokens":120,"completion_tokens":60,
        "prompt_tokens_details":{"audio_tokens":0,"image_tokens":0,"cached_tokens":0,"cache_write_tokens":0},
        "completion_tokens_details":{"reasoning_tokens":0,"audio_tokens":0,"image_tokens":0}})).with_previous(Some(current));
    let u = zero.to_token_usage().unwrap();
    assert_eq!(
        (
            u.cached_tokens,
            u.cache_write_tokens,
            u.audio_prompt_tokens,
            u.image_prompt_tokens,
            u.reasoning_tokens,
            u.audio_completion_tokens,
            u.image_completion_tokens
        ),
        (0, 0, 0, 0, 0, 0, 0)
    );
    assert!(u.cache_write_5m_tokens.is_none() && u.cache_write_1h_tokens.is_none());
    assert!(u.reported_details.unwrap().reasoning);
    // A new cache aggregate without its old split must not reuse stale metadata.
    let newer = parse(json!({"prompt_tokens":120,"completion_tokens":60,
        "prompt_tokens_details":{"cache_write_tokens":30,"cache_write_tokens_details":{"text_tokens":30,"audio_tokens":0,"image_tokens":0}}}))
    .with_previous(Some(current));
    assert!(
        newer
            .to_token_usage()
            .unwrap()
            .cache_write_5m_tokens
            .is_none()
    );
}

#[test]
fn unknown_cache_intersections_do_not_claim_complete_normalized_modal_counts() {
    let probe = parse(json!({"prompt_tokens":100,"completion_tokens":0,
        "prompt_tokens_details":{"audio_tokens":60,"image_tokens":20,"cached_tokens":40,
            "cached_tokens_details":{"audio_tokens":25}}}));
    let usage = probe.to_token_usage().unwrap();
    assert_eq!(usage.audio_prompt_tokens, 35);
    let reported = usage.reported_details.unwrap();
    assert!(reported.cache_read.audio && reported.prompt.audio);
    assert!(!reported.cache_read.image && !reported.prompt.image);
    assert_eq!(usage.total_raw(), 100);
    let complete = parse(json!({"prompt_tokens":100,"completion_tokens":0,
        "prompt_tokens_details":{"audio_tokens":60,"image_tokens":20,"cached_tokens":40,
            "cached_tokens_details":{"text_tokens":15,"audio_tokens":25}}}))
    .to_token_usage()
    .unwrap();
    assert!(complete.reported_details.unwrap().cache_read.image);
}
