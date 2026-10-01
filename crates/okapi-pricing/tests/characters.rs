//! Character unit separation preserves the monetary contract of existing TTS tariffs.
use okapi_domain::{GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, InputUnit, ModelEntry, PriceBookSource, PricingMode, RatioFp,
    TierTable, book, calculate, calculate_characters,
};
use proptest::prelude::*;

fn context() -> CalcContext {
    CalcContext {
        user: UserId::new(1),
        model: ModelCode::from("tts"),
        group: GroupCode::from("g"),
        user_multiplier: RatioFp::ONE,
        monthly_tokens: 0,
        monthly_spend_micro: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        surge_active: false,
        service_tier: None,
    }
}

fn source(pricing: PricingMode, group: RatioFp) -> PriceBookSource {
    PriceBookSource {
        epoch: 1,
        models: vec![ModelEntry {
            model: ModelCode::from("tts"),
            pricing,
            tier_ratios: vec![],
        }],
        groups: vec![GroupEntry {
            group: GroupCode::from("g"),
            ratio: group,
        }],
        overrides: vec![],
        rules: vec![],
    }
}

fn ratio(model: RatioFp) -> PricingMode {
    PricingMode::Ratio {
        model_ratio: model,
        completion_ratio: RatioFp::ONE,
        cache_ratio: RatioFp::ONE,
        cache_write_ratio: RatioFp::ONE,
        audio_ratio: RatioFp::ONE,
        audio_completion_ratio: RatioFp::ONE,
        image_ratio: RatioFp::ONE,
        modality_ratios: okapi_pricing::ModalityRatios::default(),
    }
}

#[test]
fn character_tariff_goldens_keep_amount_and_explicit_unit() {
    let tiered = PricingMode::Tiered {
        tiers: TierTable::parse("0:2,10:3").unwrap(),
        completion_ratio: RatioFp::ONE,
        cache_ratio: RatioFp::ONE,
        cache_write_ratio: RatioFp::ONE,
        audio_ratio: RatioFp::ONE,
        audio_completion_ratio: RatioFp::ONE,
        image_ratio: RatioFp::ONE,
        modality_ratios: okapi_pricing::ModalityRatios::default(),
    };
    for (mode, count, expected) in [
        (ratio(RatioFp::ONE), 11, 22),
        (ratio(RatioFp::ONE), 0, 0),
        (tiered.clone(), 9, 18),
        (tiered, 10, 30),
        (
            PricingMode::PerCall {
                price: Money::from_micros(6000),
            },
            11,
            6000,
        ),
    ] {
        let book = book::compile(source(mode, RatioFp::ONE)).unwrap();
        let quote = calculate_characters(&book, &context(), count).unwrap();
        assert_eq!(quote.amount.as_micros(), expected);
        assert_eq!(quote.original.as_micros(), expected);
        assert_eq!(quote.list_price.as_micros(), expected);
        assert_eq!(quote.discount.as_micros(), 0);
        assert_eq!(quote.snapshot.input_unit, Some(InputUnit::Characters));
        assert_eq!(quote.snapshot.input_characters, Some(count));
        let snapshot = serde_json::to_value(quote.snapshot).unwrap();
        assert_eq!(snapshot["input_unit"], "characters");
        assert_eq!(snapshot["input_characters"], count);
        let tokens = calculate(&book, &context(), TokenUsage::default()).unwrap();
        let snapshot = serde_json::to_value(tokens.snapshot).unwrap();
        assert!(snapshot.get("input_characters").is_none());
        assert!(snapshot.get("input_unit").is_none());
    }
}

proptest! {
    #[test]
    fn character_quotes_preserve_four_amounts_across_base_group_and_personal_prices(
        count in 0u32..=u32::MAX,
        base in 1i64..=5_000_000,
        model in 0i64..=5_000_000,
        group in 0i64..=5_000_000,
        user in 0i64..=5_000_000,
    ) {
        let book=book::compile_with_base(source(ratio(RatioFp::from_scaled(model).unwrap()), RatioFp::from_scaled(group).unwrap()),base).unwrap();
        let mut ctx=context();
        ctx.user_multiplier=RatioFp::from_scaled(user).unwrap();
        let old=calculate(&book,&ctx,TokenUsage {prompt_tokens:count,..Default::default()}).unwrap();
        let new=calculate_characters(&book,&ctx,count).unwrap();
        prop_assert_eq!((new.amount,new.original,new.discount,new.list_price),(old.amount,old.original,old.discount,old.list_price));
        prop_assert_eq!(new.snapshot.input_characters,Some(count));
        prop_assert_eq!(new.snapshot.input_unit,Some(InputUnit::Characters));
    }
}

#[test]
fn character_quotes_keep_service_tier_group_personal_rule_and_site_base_prices() {
    use okapi_pricing::{PricingRule, RuleKind, RuleScope, Stacking};
    let mut config = source(ratio("1.5".parse().unwrap()), "0.8".parse().unwrap());
    config.models[0].tier_ratios = vec![("flex".into(), "0.5".parse().unwrap())];
    config.rules.push(PricingRule {
        code: "discount".into(),
        kind: RuleKind::Discount,
        multiplier: "0.9".parse().unwrap(),
        scope: RuleScope::default(),
        priority: 0,
        stacking: Stacking::Stackable,
        valid_from: None,
        valid_to: None,
    });
    let book = book::compile_with_base(config, 5_000_000).unwrap();
    let mut ctx = context();
    ctx.service_tier = Some("flex".into());
    ctx.user_multiplier = "0.5".parse().unwrap();
    let quote = calculate_characters(&book, &ctx, 1000).unwrap();
    assert_eq!(quote.list_price.as_micros(), 3750);
    assert_eq!(quote.original.as_micros(), 3000);
    assert_eq!(quote.amount.as_micros(), 1350);
    assert_eq!(quote.discount.as_micros(), 1650);
    assert_eq!(quote.snapshot.service_tier.as_deref(), Some("flex"));
    assert_eq!(quote.snapshot.rules.len(), 1);
    assert_eq!(quote.snapshot.rules[0].code, "discount");
    assert_eq!(quote.snapshot.input_unit, Some(InputUnit::Characters));
    assert_eq!(quote.snapshot.input_characters, Some(1000));
}
