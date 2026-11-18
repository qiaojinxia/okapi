use okapi_domain::{GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, ModelEntry, PriceBookSource, PricingMode, RatioFp, calculate,
};
fn source(pricing: PricingMode) -> PriceBookSource {
    PriceBookSource {
        epoch: 1,
        models: vec![ModelEntry {
            model: ModelCode::from("fixture"),
            pricing,
            tier_ratios: vec![],
        }],
        groups: vec![GroupEntry {
            group: GroupCode::from("default"),
            ratio: RatioFp::ONE,
        }],
        overrides: vec![],
        rules: vec![],
    }
}
#[test]
fn negative_fixed_price_is_rejected_by_library_compilation() {
    assert!(
        okapi_pricing::book::compile(source(PricingMode::PerCall {
            price: Money::from_micros(-100)
        }))
        .is_err()
    );
}
#[test]
fn extreme_audio_ratio_returns_error_without_panicking() {
    let high = RatioFp::from_scaled(i64::MAX).unwrap();
    let mode = PricingMode::Ratio {
        model_ratio: RatioFp::ONE,
        completion_ratio: RatioFp::ONE,
        cache_ratio: RatioFp::ONE,
        cache_write_ratio: RatioFp::ONE,
        audio_ratio: high,
        audio_completion_ratio: high,
        image_ratio: RatioFp::ONE,
        modality_ratios: okapi_pricing::ModalityRatios::default(),
    };
    let book = okapi_pricing::book::compile(source(mode)).unwrap();
    let context = CalcContext {
        user: UserId::new(1),
        model: ModelCode::from("fixture"),
        group: GroupCode::from("default"),
        user_multiplier: RatioFp::ONE,
        monthly_tokens: 0,
        monthly_spend_micro: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        utc_offset_seconds: 0,
        surge_active: false,
        service_tier: None,
    };
    let outcome = std::panic::catch_unwind(|| {
        calculate(
            &book,
            &context,
            TokenUsage {
                completion_tokens: i32::MAX as u32,
                audio_completion_tokens: i32::MAX as u32,
                ..Default::default()
            },
        )
    });
    assert!(outcome.is_ok());
    assert!(outcome.unwrap().is_err());
}
