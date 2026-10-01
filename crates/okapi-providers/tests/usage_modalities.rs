use okapi_api::UsageProbe;
use okapi_domain::{CacheModalities, TokenUsage, UpstreamTokenCounts};
use okapi_providers::{ChatEvent, convert::openai_to_gemini, gemini::MetaScanner, responses};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/multimodal_usage.json")).unwrap()
}

fn expected() -> TokenUsage {
    TokenUsage {
        reported_details: Some(okapi_domain::TokenDetailsReported {
            prompt: okapi_domain::ModalitiesReported {
                audio: true,
                image: true,
            },
            completion: okapi_domain::ModalitiesReported {
                audio: true,
                image: true,
            },
            cache_read: okapi_domain::ModalitiesReported {
                audio: true,
                image: true,
            },
            cache_write: okapi_domain::ModalitiesReported::default(),
            reasoning: true,
        }),
        upstream_usage: Some(UpstreamTokenCounts {
            prompt_tokens: Some(1_000),
            completion_tokens: Some(400),
        }),
        prompt_tokens: 1_000,
        completion_tokens: 400,
        cached_tokens: 300,
        cache_read_reported: true,
        cache_read_modalities: Some(CacheModalities {
            audio_tokens: 150,
            image_tokens: 100,
        }),
        audio_prompt_tokens: 350,
        image_prompt_tokens: 200,
        audio_completion_tokens: 100,
        image_completion_tokens: 200,
        reasoning_tokens: 20,
        ..TokenUsage::default()
    }
}

#[test]
fn protocols_produce_identical_exclusive_counts_without_double_counting() {
    let f = fixture();
    let chat: UsageProbe = serde_json::from_value(f["chat"].clone()).unwrap();
    let responses = responses::usage_from_responses(Some(&f["responses"])).unwrap();
    let gemini = openai_to_gemini::usage_from_gemini(Some(&f["gemini"])).unwrap();
    for probe in [chat, responses, gemini] {
        let u = probe.to_token_usage().unwrap();
        assert_eq!(u, expected());
        assert_eq!(u.total_raw(), 1_400);
        assert_eq!(u.prompt_uncached(), 150);
        assert_eq!(u.text_completion(), 100);
    }
    let converted = okapi_providers::convert::gemini_to_openai::gemini_usage_json(chat);
    assert_eq!(
        openai_to_gemini::usage_from_gemini(Some(&converted))
            .unwrap()
            .to_token_usage()
            .unwrap(),
        expected()
    );
}

#[test]
fn full_cache_and_single_modality_cache_can_be_inferred_without_guessing() {
    let mut full = fixture()["chat"].clone();
    full["prompt_tokens_details"]["cached_tokens"] = json!(1_000);
    full["prompt_tokens_details"]
        .as_object_mut()
        .unwrap()
        .remove("cached_tokens_details");
    let u = serde_json::from_value::<UsageProbe>(full)
        .unwrap()
        .to_token_usage()
        .unwrap();
    assert_eq!(
        (
            u.audio_prompt_tokens,
            u.image_prompt_tokens,
            u.prompt_uncached()
        ),
        (0, 0, 0)
    );
    assert_eq!(
        u.cache_read_modalities,
        Some(CacheModalities {
            audio_tokens: 500,
            image_tokens: 300
        })
    );
    let single: UsageProbe = serde_json::from_value(json!({"prompt_tokens":100,"completion_tokens":0,"prompt_tokens_details":{"audio_tokens":100,"cached_tokens":30}})).unwrap();
    let u = single.to_token_usage().unwrap();
    assert_eq!(u.audio_prompt_tokens, 70);
    assert_eq!(u.cache_read_modalities.unwrap().audio_tokens, 30);
}

#[test]
fn cache_writes_subtract_their_modal_intersections_too() {
    let mut raw = fixture()["chat"].clone();
    raw["prompt_tokens_details"]["cache_write_tokens"] = json!(100);
    raw["prompt_tokens_details"]["cache_write_tokens_details"] =
        json!({"text_tokens":30,"audio_tokens":20,"image_tokens":50});
    let u = serde_json::from_value::<UsageProbe>(raw)
        .unwrap()
        .to_token_usage()
        .unwrap();
    assert_eq!(
        (
            u.audio_prompt_tokens,
            u.image_prompt_tokens,
            u.prompt_uncached()
        ),
        (330, 150, 120)
    );
    assert_eq!(u.cache_write_text(), 30);
    assert!(u.cache_write_reported);
}

#[test]
fn invalid_or_ambiguous_usage_is_present_but_not_billable() {
    for (pointer, value) in [
        ("/prompt_tokens", json!(-1)),
        ("/prompt_tokens", json!(1.5)),
        ("/total_tokens", json!(1)),
        ("/prompt_tokens_details/cached_tokens", json!(1001)),
        (
            "/prompt_tokens_details/cached_tokens_details/audio_tokens",
            json!(501),
        ),
        ("/prompt_tokens_details/cached_tokens_details", Value::Null),
        ("/completion_tokens_details/audio_tokens", json!(401)),
        ("/completion_tokens_details/reasoning_tokens", json!(401)),
    ] {
        let mut raw = fixture()["chat"].clone();
        *raw.pointer_mut(pointer).unwrap() = value;
        let probe: UsageProbe = serde_json::from_value(raw).unwrap();
        assert!(probe.to_token_usage().is_err(), "{pointer}");
    }
    for raw in [json!(false), json!({}), json!({"input_tokens":-1})] {
        assert!(
            responses::usage_from_responses(Some(&raw))
                .unwrap()
                .to_token_usage()
                .is_err(),
            "{raw}"
        );
    }
}

#[test]
fn gemini_missing_and_invalid_metadata_cannot_imply_a_known_zero() {
    assert!(openai_to_gemini::usage_from_gemini(None).is_none());
    assert!(openai_to_gemini::usage_from_gemini(Some(&Value::Null)).is_none());
    for (pointer, value) in [
        ("/promptTokenCount", json!(-1)),
        ("/candidatesTokenCount", json!(u32::MAX)),
        ("/totalTokenCount", json!(1)),
        ("/cacheTokensDetails/0/tokenCount", json!(51)),
        ("/promptTokensDetails/0/modality", json!("AUDIO")),
        ("/candidatesTokensDetails/0/tokenCount", json!(999)),
    ] {
        let mut raw = fixture()["gemini"].clone();
        *raw.pointer_mut(pointer).unwrap() = value;
        assert!(
            openai_to_gemini::usage_from_gemini(Some(&raw))
                .unwrap()
                .to_token_usage()
                .is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn gemini_native_and_converted_streams_preserve_modal_usage() {
    let body = json!({"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":fixture()["gemini"]});
    let mut native = MetaScanner::new();
    let mut converted = openai_to_gemini::GeminiStreamState::new("fixture");
    for events in [
        native.scan(Ok(body.to_string())),
        converted.step(Ok(body.to_string())),
    ] {
        let probe = events
            .into_iter()
            .find_map(|e| match e.unwrap() {
                ChatEvent::Data { usage, .. } => usage,
                ChatEvent::Done => None,
            })
            .unwrap();
        assert_eq!(probe.to_token_usage().unwrap(), expected());
    }
    let (bytes, probe) = openai_to_gemini::response_gemini_to_openai(
        &bytes::Bytes::from(body.to_string()),
        "fixture",
    )
    .unwrap();
    assert_eq!(probe.unwrap().to_token_usage().unwrap(), expected());
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    let reparsed: UsageProbe = serde_json::from_value(wire["usage"].clone()).unwrap();
    assert_eq!(reparsed.to_token_usage().unwrap(), expected());
}

#[test]
fn gemini_partial_arrays_preserve_unknown_axes_and_explicit_zero_in_both_directions() {
    use okapi_providers::convert::gemini_to_openai::gemini_usage_json;
    for (details, audio, image) in [
        (json!([{"modality":"AUDIO","tokenCount":0}]), true, false),
        (json!([{"modality":"TEXT","tokenCount":100}]), true, true),
        (json!([{"modality":"TEXT","tokenCount":20}]), false, false),
    ] {
        let raw = json!({"promptTokenCount":100,"candidatesTokenCount":50,"thoughtsTokenCount":0,
            "promptTokensDetails":details});
        let probe = openai_to_gemini::usage_from_gemini(Some(&raw)).unwrap();
        let usage = probe.to_token_usage().unwrap();
        let observed = usage.reported_details.unwrap();
        assert_eq!(
            (observed.prompt.audio, observed.prompt.image),
            (audio, image)
        );
        assert!(observed.reasoning);
        assert!(!observed.completion.audio && !observed.completion.image);
        let converted = gemini_usage_json(probe);
        assert_eq!(converted["thoughtsTokenCount"], 0);
        assert_eq!(
            openai_to_gemini::usage_from_gemini(Some(&converted))
                .unwrap()
                .to_token_usage()
                .unwrap(),
            usage
        );
    }
    let absent = openai_to_gemini::usage_from_gemini(Some(
        &json!({"promptTokenCount":100,"candidatesTokenCount":50}),
    ))
    .unwrap();
    let converted = gemini_usage_json(absent);
    assert!(converted.get("thoughtsTokenCount").is_none());
    assert!(converted.get("promptTokensDetails").is_none());
}
