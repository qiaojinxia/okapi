//! Cache/modality intersections are exclusive charges, with exact integer fixtures.
use okapi_domain::{CacheModalities, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, ModalityRatios, ModelEntry, OverrideEntry, OverrideSpec, PriceBook,
    PriceBookSource, PricingMode, RatioFp, TierTable, book, calculate,
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn mode(rates: ModalityRatios) -> PricingMode {
    PricingMode::Ratio {
        model_ratio: "2.5".parse().unwrap(),
        completion_ratio: "4".parse().unwrap(),
        cache_ratio: "0.25".parse().unwrap(),
        cache_write_ratio: "1.25".parse().unwrap(),
        audio_ratio: "16".parse().unwrap(),
        audio_completion_ratio: "2".parse().unwrap(),
        image_ratio: "1.6".parse().unwrap(),
        modality_ratios: rates,
    }
}
fn setup(pricing: PricingMode, override_spec: Option<OverrideSpec>) -> (PriceBook, CalcContext) {
    let model = ModelCode::from("modal");
    let group = GroupCode::from("g");
    let user = UserId::new(1);
    let book = book::compile(PriceBookSource {
        epoch: 17,
        models: vec![ModelEntry {
            model: model.clone(),
            pricing,
            tier_ratios: vec![],
        }],
        groups: vec![GroupEntry {
            group: group.clone(),
            ratio: RatioFp::ONE,
        }],
        overrides: override_spec
            .into_iter()
            .map(|spec| OverrideEntry {
                user,
                model: model.clone(),
                spec,
            })
            .collect(),
        rules: vec![],
    })
    .unwrap();
    (
        book,
        CalcContext {
            user,
            model,
            group,
            user_multiplier: RatioFp::ONE,
            monthly_tokens: 0,
            monthly_spend_micro: 0,
            local_minute_of_day: 0,
            now_unix: 0,
            utc_offset_seconds: 0,
            surge_active: false,
            service_tier: None,
        },
    )
}
fn explicit() -> ModalityRatios {
    ModalityRatios::parse(&json!({"image_cache_read":"0.7","audio_cache_read":"0.3",
        "image_cache_write":"2.2","audio_cache_write":"3.3","image_output":"7"}))
    .unwrap()
}
fn segments(n: [u32; 12]) -> TokenUsage {
    TokenUsage {
        prompt_tokens: n[..9].iter().sum(),
        image_prompt_tokens: n[1],
        audio_prompt_tokens: n[2],
        cached_tokens: n[3..6].iter().sum(),
        cache_write_tokens: n[6..9].iter().sum(),
        cache_read_modalities: Some(CacheModalities {
            image_tokens: n[4],
            audio_tokens: n[5],
        }),
        cache_write_modalities: Some(CacheModalities {
            image_tokens: n[7],
            audio_tokens: n[8],
        }),
        completion_tokens: n[9..].iter().sum(),
        image_completion_tokens: n[10],
        audio_completion_tokens: n[11],
        cache_read_reported: true,
        cache_write_reported: true,
        ..TokenUsage::default()
    }
}

#[test]
fn pinned_sub2api_image_cache_reference_costs() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/image_cache_parity.json")).unwrap();
    let rates =
        ModalityRatios::parse(&json!({"image_cache_read":"0.4","image_output":"6"})).unwrap();
    let (book, ctx) = setup(mode(rates), None);
    for case in fixture["cases"].as_array().unwrap() {
        let n = |key: &str| u32::try_from(case[key].as_u64().unwrap()).unwrap();
        let usage = TokenUsage {
            prompt_tokens: n("input"),
            cached_tokens: n("cached"),
            cache_read_modalities: Some(CacheModalities {
                image_tokens: n("cached_image"),
                audio_tokens: 0,
            }),
            image_prompt_tokens: n("image") - n("cached_image"),
            completion_tokens: n("output"),
            image_completion_tokens: n("output"),
            ..TokenUsage::default()
        };
        let quote = calculate(&book, &ctx, usage).unwrap();
        assert_eq!(quote.amount.as_micros(), case["micro"].as_i64().unwrap());
        assert_eq!(quote.original, quote.amount);
        assert_eq!(quote.list_price, quote.amount);
        assert_eq!(quote.discount.as_micros(), 0);
    }
}

#[test]
fn explicit_prices_and_all_twelve_segments_survive_absolute_override() {
    let usage = segments([8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52]);
    for spec in [
        None,
        Some(OverrideSpec::Absolute {
            input_per_1m: Money::from_micros(5_000_000),
            output_per_1m: Money::from_micros(20_000_000),
            cache_ratio: "0.25".parse().unwrap(),
            cache_write_ratio: "1.25".parse().unwrap(),
        }),
    ] {
        let (book, ctx) = setup(mode(explicit()), spec);
        let quote = calculate(&book, &ctx, usage).unwrap();
        assert_eq!(quote.amount.as_micros(), 13703);
        let snapshot = serde_json::to_value(quote.snapshot).unwrap();
        assert_eq!(
            snapshot["modality_ratios"]["image_cache_read"].to_string(),
            "0.7"
        );
        assert_eq!(
            snapshot["modality_ratios"]["audio_cache_write"].to_string(),
            "3.3"
        );
        assert_eq!(
            snapshot["cache_read_modalities"],
            json!({"audio_tokens":28,"image_tokens":24})
        );
        assert_eq!(
            snapshot["cache_write_modalities"],
            json!({"audio_tokens":40,"image_tokens":36})
        );
        assert_eq!(snapshot["image_completion_tokens"], 48);
    }
}

#[test]
fn tiered_prices_use_the_same_modal_matrix() {
    let pricing = PricingMode::Tiered {
        completion_ratio: "4".parse().unwrap(),
        cache_ratio: "0.25".parse().unwrap(),
        cache_write_ratio: "1.25".parse().unwrap(),
        audio_ratio: "16".parse().unwrap(),
        audio_completion_ratio: "2".parse().unwrap(),
        image_ratio: "1.6".parse().unwrap(),
        modality_ratios: explicit(),
        tiers: TierTable::parse("0:5,1000:10").unwrap(),
    };
    let (book, ctx) = setup(pricing, None);
    let quote = calculate(
        &book,
        &ctx,
        segments([8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52]),
    )
    .unwrap();
    assert_eq!(quote.amount.as_micros(), 13703);
    assert_eq!(quote.snapshot.mode, "tiered");
}

#[test]
fn absent_prices_use_modal_times_cache_and_completion_fallbacks() {
    let (book, ctx) = setup(mode(ModalityRatios::default()), None);
    let quote = calculate(&book, &ctx, segments([0, 0, 0, 0, 4, 4, 0, 4, 4, 0, 4, 0])).unwrap();
    // 4 * (2 cached image + 20 cached audio + 10 written image + 100 written audio + 20 image output).
    assert_eq!(quote.amount.as_micros(), 608);
    let snapshot = serde_json::to_value(quote.snapshot).unwrap();
    assert_eq!(
        snapshot["modality_ratios"]["image_cache_read"].to_string(),
        "0.4"
    );
    assert_eq!(snapshot["modality_ratios"]["image_output"], 4);
}

#[test]
fn strict_price_configuration_and_unused_overflow() {
    for bad in [
        json!(null),
        json!([]),
        json!({"image_cache_read":1}),
        json!({"wrong":"1"}),
        json!({"image_output":"-1"}),
        json!({"audio_cache_read":"NaN"}),
    ] {
        assert!(ModalityRatios::parse(&bad).is_err());
    }
    let mut pricing = mode(ModalityRatios::default());
    if let PricingMode::Ratio {
        image_ratio,
        cache_ratio,
        ..
    } = &mut pricing
    {
        *image_ratio = RatioFp::from_scaled(i64::MAX).unwrap();
        *cache_ratio = RatioFp::from_scaled(i64::MAX).unwrap();
    }
    let (book, ctx) = setup(pricing, None);
    assert_eq!(
        calculate(
            &book,
            &ctx,
            TokenUsage {
                prompt_tokens: 1,
                ..TokenUsage::default()
            }
        )
        .unwrap()
        .amount
        .as_micros(),
        5
    );
    assert!(calculate(&book, &ctx, segments([0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0])).is_err());
}

proptest! {
    #[test]
    fn independent_prices_cover_each_token_exactly_once(n in prop::array::uniform12(0u32..=100_000)) {
        let (book, ctx) = setup(mode(explicit()), None);
        let quote = calculate(&book, &ctx, segments(n)).unwrap();
        // Independent price table in quarter-micro USD, not engine ratio arithmetic.
        let prices = [20u64,32,320,5,14,6,25,44,66,80,140,640];
        let expected = n.into_iter().zip(prices).map(|(n,p)|u64::from(n)*p).sum::<u64>()/4;
        prop_assert_eq!(quote.amount.as_micros(), i64::try_from(expected).unwrap());
        prop_assert_eq!(quote.list_price, quote.amount);
        prop_assert_eq!(quote.original, quote.amount);
        prop_assert_eq!(quote.discount.as_micros(), 0);
    }
}
