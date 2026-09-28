use okapi_domain::{GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, ModelEntry, OverrideEntry, OverrideSpec, PriceBookSource, PricingMode,
    RatioFp, TierTable, book, calculate,
};

fn ratio() -> PricingMode {
    PricingMode::Ratio {
        model_ratio: "1.5".parse().unwrap(),
        completion_ratio: "4".parse().unwrap(),
        cache_ratio: "0.25".parse().unwrap(),
        cache_write_ratio: "1.25".parse().unwrap(),
        audio_ratio: "2".parse().unwrap(),
        audio_completion_ratio: "3".parse().unwrap(),
        image_ratio: "2".parse().unwrap(),
        modality_ratios: okapi_pricing::ModalityRatios::default(),
    }
}

fn source(pricing: PricingMode) -> PriceBookSource {
    PriceBookSource {
        epoch: 9,
        models: vec![ModelEntry {
            model: ModelCode::from("m"),
            pricing,
            tier_ratios: vec![],
        }],
        groups: vec![GroupEntry {
            group: GroupCode::from("g"),
            ratio: "0.8".parse().unwrap(),
        }],
        overrides: vec![],
        rules: vec![],
    }
}

fn context() -> CalcContext {
    CalcContext {
        user: UserId::new(1),
        model: ModelCode::from("m"),
        group: GroupCode::from("g"),
        user_multiplier: "0.5".parse().unwrap(),
        monthly_tokens: 0,
        monthly_spend_micro: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        surge_active: false,
        service_tier: None,
    }
}

#[test]
fn custom_base_scales_cache_modal_group_and_personal_prices_and_snapshot() {
    let usage = TokenUsage {
        prompt_tokens: 10_000,
        cached_tokens: 2_000,
        cache_write_tokens: 1_000,
        audio_prompt_tokens: 1_000,
        image_prompt_tokens: 1_000,
        completion_tokens: 2_000,
        audio_completion_tokens: 500,
        ..Default::default()
    };
    let default = calculate(&book::compile(source(ratio())).unwrap(), &context(), usage).unwrap();
    let changed = calculate(
        &book::compile_with_base(source(ratio()), 5_000_000).unwrap(),
        &context(),
        usage,
    )
    .unwrap();
    assert_eq!(
        changed.amount.as_micros(),
        default.amount.as_micros() * 5 / 2
    );
    assert_eq!(
        changed.original.as_micros(),
        default.original.as_micros() * 5 / 2
    );
    assert_eq!(
        changed.list_price.as_micros(),
        default.list_price.as_micros() * 5 / 2
    );
    assert_eq!(
        changed
            .snapshot
            .final_unit_price_input_per_1m_usd
            .unwrap()
            .as_micros(),
        3_000_000
    );
    let snapshot = serde_json::to_value(changed.snapshot).unwrap();
    assert_eq!(snapshot["base_price_per_1m_usd"], 5);
    assert_eq!(
        default.snapshot.base_price_per_1m_usd.unwrap().as_micros(),
        2_000_000
    );
}

#[test]
fn absolute_overrides_tiers_and_per_call_do_not_follow_site_base() {
    let tiered = PricingMode::Tiered {
        tiers: TierTable::parse("0:3,128000:6").unwrap(),
        completion_ratio: RatioFp::ONE,
        cache_ratio: RatioFp::ONE,
        cache_write_ratio: RatioFp::ONE,
        audio_ratio: RatioFp::ONE,
        audio_completion_ratio: RatioFp::ONE,
        image_ratio: RatioFp::ONE,
        modality_ratios: okapi_pricing::ModalityRatios::default(),
    };
    let mut absolute = source(ratio());
    absolute.overrides.push(OverrideEntry {
        user: UserId::new(1),
        model: ModelCode::from("m"),
        spec: OverrideSpec::Absolute {
            input_per_1m: Money::from_micros(3_000_000),
            output_per_1m: Money::from_micros(9_000_000),
            cache_ratio: RatioFp::ONE,
            cache_write_ratio: RatioFp::ONE,
        },
    });
    for config in [
        absolute,
        source(tiered),
        source(PricingMode::PerCall {
            price: Money::from_micros(30_000),
        }),
    ] {
        let usage = TokenUsage {
            prompt_tokens: 200_000,
            completion_tokens: 1_000,
            ..Default::default()
        };
        let baseline =
            calculate(&book::compile(config.clone()).unwrap(), &context(), usage).unwrap();
        for base in [1, 500_000, 5_000_000, book::MAX_BASE_PRICE_PER_1M_MICRO] {
            let updated = calculate(
                &book::compile_with_base(config.clone(), base).unwrap(),
                &context(),
                usage,
            )
            .unwrap();
            assert_eq!(baseline.amount, updated.amount);
            assert_eq!(baseline.list_price, updated.list_price);
        }
    }
}

#[test]
fn fractional_micro_per_token_is_not_truncated_early() {
    let mut config = source(ratio());
    config.groups[0].ratio = RatioFp::ONE;
    let mut ctx = context();
    ctx.user_multiplier = RatioFp::ONE;
    let usage = TokenUsage {
        prompt_tokens: 1_000_000,
        ..Default::default()
    };
    let quote = calculate(&book::compile_with_base(config, 1).unwrap(), &ctx, usage).unwrap();
    assert_eq!(quote.amount.as_micros(), 1); // floor(1 micro × model ratio 1.5)
}

#[test]
fn invalid_bases_are_rejected() {
    for base in [-1, 0, book::MAX_BASE_PRICE_PER_1M_MICRO + 1, i64::MAX] {
        assert!(book::compile_with_base(source(ratio()), base).is_err());
    }
}
