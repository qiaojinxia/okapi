use okapi_api::UsageProbe;
use okapi_domain::{TokenUsage, UpstreamTokenCounts};
use serde_json::{Value, json};

fn parse(value: Value) -> UsageProbe {
    serde_json::from_value(value).unwrap()
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
