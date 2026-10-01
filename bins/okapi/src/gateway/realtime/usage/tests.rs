use super::*;
use serde_json::json;

fn event(id: &str, usage: &Value) -> String {
    json!({"type":"response.done", "response":{"id":id,"usage":usage}}).to_string()
}

fn official_usage() -> Value {
    // https://developers.openai.com/api/docs/guides/voice-latency-cost#example
    json!({"input_tokens":132,"output_tokens":121,"total_tokens":253,
        "input_token_details":{"text_tokens":119,"audio_tokens":13,"image_tokens":0,
            "cached_tokens":64,"cached_tokens_details":{"text_tokens":64,"audio_tokens":0,"image_tokens":0}},
        "output_token_details":{"text_tokens":30,"audio_tokens":91}})
}

#[test]
fn official_response_and_replays_preserve_all_token_axes() {
    let mut meter = Meter::default();
    let first = event("r1", &official_usage());
    meter.observe(&first).unwrap();
    meter.observe(&first).unwrap();
    assert_eq!(meter.responses, 1);
    assert_eq!(meter.usage.total_raw(), 253);
    assert_eq!(meter.usage.prompt_uncached(), 55);
    assert_eq!(meter.usage.audio_prompt_tokens, 13);
    assert_eq!(meter.usage.text_completion(), 30);
    assert_eq!(meter.usage.audio_completion_tokens, 91);
    assert_eq!(meter.usage.cached_text(), 64);
    assert!(meter.usage.cache_read_reported);
    assert!(!meter.usage.cache_write_reported);
    meter.observe(&event("r2", &official_usage())).unwrap();
    assert_eq!(meter.responses, 2);
    assert_eq!(meter.usage.total_raw(), 506);
    assert_eq!(meter.usage.cached_tokens, 128);
    assert_eq!(meter.usage.audio_completion_tokens, 182);
}

#[test]
fn cached_audio_and_image_are_subsets_not_extra_input() {
    let mut meter = Meter::default();
    meter.observe(&event("r1", &json!({"input_tokens":100,"output_tokens":50,
        "input_token_details":{"text_tokens":20,"audio_tokens":60,"image_tokens":20,
            "cached_tokens":40,"cached_tokens_details":{"text_tokens":10,"audio_tokens":25,"image_tokens":5},
            "cache_write_tokens":10,"cache_write_tokens_details":{"text_tokens":0,"audio_tokens":5,"image_tokens":5}},
        "output_token_details":{"text_tokens":10,"audio_tokens":40}}))).unwrap();
    let usage = meter.usage;
    assert_eq!(usage.total_raw(), 150);
    assert_eq!(usage.prompt_uncached(), 10);
    assert_eq!(
        (usage.audio_prompt_tokens, usage.image_prompt_tokens),
        (30, 10)
    );
    assert_eq!(usage.cache_read_modalities.unwrap().audio_tokens, 25);
    assert_eq!(usage.cache_write_modalities.unwrap().image_tokens, 5);
    assert_eq!((usage.cached_text(), usage.cache_write_text()), (10, 0));
}

#[test]
fn one_missing_report_keeps_whole_session_cache_coverage_unknown() {
    for missing_first in [false, true] {
        let known = official_usage();
        let missing = json!({"input_tokens":10,"output_tokens":20});
        let mut meter = Meter::default();
        let inputs = if missing_first {
            [missing, known]
        } else {
            [known, missing]
        };
        for (index, usage) in inputs.into_iter().enumerate() {
            meter.observe(&event(&index.to_string(), &usage)).unwrap();
        }
        assert_eq!(meter.usage.total_raw(), 283);
        assert_eq!(meter.usage.cached_tokens, 64);
        assert!(!meter.usage.cache_read_reported);
        assert!(!meter.usage.cache_write_reported);
    }
}

#[test]
fn invalid_or_overflowing_usage_cannot_corrupt_verified_prefix() {
    let mut invalid = vec![
        Value::Null,
        json!({"input_tokens":-1,"output_tokens":1}),
        json!({"input_tokens":1.5,"output_tokens":1}),
        json!({"input_tokens":2_147_483_648_u64,"output_tokens":1}),
        json!({"input_tokens":2_147_483_647_u64,"output_tokens":1}),
    ];
    for (path, value) in [
        ("/total_tokens", json!(254)),
        (
            "/input_token_details/cached_tokens_details/audio_tokens",
            json!(20),
        ),
        ("/input_token_details/cached_tokens_details", Value::Null),
        ("/input_token_details/cached_tokens", json!(133)),
        ("/output_token_details/audio_tokens", json!(122)),
    ] {
        let mut usage = official_usage();
        *usage.pointer_mut(path).unwrap() = value;
        invalid.push(usage);
    }
    for usage in invalid {
        let mut meter = Meter::default();
        meter.observe(&event("r1", &official_usage())).unwrap();
        let prefix = meter.usage;
        assert!(meter.observe(&event("bad", &usage)).is_err(), "{usage}");
        assert_eq!(meter.usage, prefix);
        assert_eq!(meter.responses, 1);
        assert!(!meter.seen.contains("bad"));
    }
}

#[test]
fn details_and_original_totals_remain_observed_only_when_every_response_reports_them() {
    let mut meter = Meter::default();
    meter.observe(&event("known", &official_usage())).unwrap();
    assert_eq!(
        (meter.usage.prompt_source(), meter.usage.completion_source()),
        ("upstream", "upstream")
    );
    let reported = meter.usage.reported_details.unwrap();
    assert!(reported.prompt.audio && reported.prompt.image);
    assert!(reported.completion.audio && reported.completion.image);
    assert!(reported.cache_read.audio && reported.cache_read.image);
    assert!(!reported.reasoning);
    meter
        .observe(&event(
            "unknown",
            &json!({"input_tokens":10,"output_tokens":20}),
        ))
        .unwrap();
    let reported = meter.usage.reported_details.unwrap();
    assert!(!reported.prompt.audio && !reported.completion.audio && !reported.cache_read.audio);
    assert_eq!(
        (meter.usage.prompt_source(), meter.usage.completion_source()),
        ("upstream", "upstream")
    );
    assert_eq!(
        (meter.usage.prompt_tokens, meter.usage.completion_tokens),
        (142, 141)
    );
}
