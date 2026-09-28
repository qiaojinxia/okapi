//! Direct Images endpoints: the output axis is entirely image output.
use okapi_domain::{GroupCode, ModelCode, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, ModelEntry, PriceBook, PriceBookSource, PricingMode, RatioFp, book,
    calculate,
};
use proptest::prelude::*;

fn setup() -> (PriceBook, CalcContext) {
    let model = ModelCode::from("direct-image");
    let group = GroupCode::from("default");
    let book = book::compile(PriceBookSource {
        epoch: 42,
        models: vec![ModelEntry {
            model: model.clone(),
            pricing: PricingMode::Ratio {
                model_ratio: "2.5".parse().unwrap(),
                completion_ratio: "6".parse().unwrap(),
                image_ratio: "1.6".parse().unwrap(),
                modality_ratios: okapi_pricing::ModalityRatios::default(),
                cache_ratio: RatioFp::ONE,
                cache_write_ratio: RatioFp::ONE,
                audio_ratio: RatioFp::ONE,
                audio_completion_ratio: RatioFp::ONE,
            },
            tier_ratios: vec![],
        }],
        groups: vec![GroupEntry {
            group: group.clone(),
            ratio: RatioFp::ONE,
        }],
        overrides: vec![],
        rules: vec![],
    })
    .unwrap();
    (
        book,
        CalcContext {
            user: UserId::new(1),
            model,
            group,
            user_multiplier: RatioFp::ONE,
            monthly_tokens: 0,
            monthly_spend_micro: 0,
            local_minute_of_day: 0,
            now_unix: 0,
            surge_active: false,
            service_tier: None,
        },
    )
}

#[test]
fn direct_image_reference_fixture_has_exact_three_axis_cost() {
    let (book, context) = setup();
    let quote = calculate(
        &book,
        &context,
        TokenUsage {
            prompt_tokens: 100,
            image_prompt_tokens: 80,
            completion_tokens: 200,
            ..TokenUsage::default()
        },
    )
    .unwrap();
    // Explicit fixture prices: text 5, image input 8, image output 30 micro/token.
    assert_eq!(quote.amount.as_micros(), 6740);
    assert_eq!(quote.original.as_micros(), 6740);
    assert_eq!(quote.list_price.as_micros(), 6740);
    assert_eq!(quote.discount.as_micros(), 0);
    assert_eq!(quote.snapshot.image_ratio.unwrap().to_string(), "1.6");
    assert_eq!(quote.snapshot.completion_ratio.unwrap().to_string(), "6");
}

proptest! {
    #[test]
    fn each_direct_image_axis_matches_independent_integer_prices(text in 0u32..=1_000_000,image in 0u32..=1_000_000,output in 0u32..=1_000_000) {
        let (book,context)=setup();
        let quote=calculate(&book,&context,TokenUsage{
            prompt_tokens:text+image,image_prompt_tokens:image,completion_tokens:output,..TokenUsage::default()
        }).unwrap();
        let expected=i64::from(text)*5+i64::from(image)*8+i64::from(output)*30;
        prop_assert_eq!(quote.amount.as_micros(),expected);
        prop_assert_eq!(quote.list_price.as_micros(),expected);
        prop_assert_eq!(quote.discount.as_micros(),0);
    }
}
