use okapi_domain::{CacheModalities, GroupCode, ModelCode, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, ModalityRatios, ModelEntry, PriceBookSource, PricingMode, RatioFp,
    book, calculate,
};
use serde_json::json;

fn quote(
    usage: TokenUsage,
    rates: &serde_json::Value,
) -> Result<okapi_pricing::Quote, okapi_pricing::PricingError> {
    let model = ModelCode::from("ttl");
    let group = GroupCode::from("default");
    let book = book::compile(PriceBookSource {
        epoch: 1,
        models: vec![ModelEntry {
            model: model.clone(),
            tier_ratios: vec![],
            pricing: PricingMode::Ratio {
                model_ratio: RatioFp::ONE,
                completion_ratio: RatioFp::ONE,
                cache_ratio: RatioFp::ONE,
                cache_write_ratio: "1.25".parse().unwrap(),
                audio_ratio: RatioFp::ONE,
                audio_completion_ratio: RatioFp::ONE,
                image_ratio: RatioFp::ONE,
                modality_ratios: ModalityRatios::parse(rates).unwrap(),
            },
        }],
        groups: vec![GroupEntry {
            group: group.clone(),
            ratio: RatioFp::ONE,
        }],
        overrides: vec![],
        rules: vec![],
    })
    .unwrap();
    calculate(
        &book,
        &CalcContext {
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
        usage,
    )
}

fn usage() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 1000,
        cache_write_tokens: 100,
        cache_write_5m_tokens: Some(60),
        cache_write_1h_tokens: Some(40),
        cache_write_reported: true,
        ..TokenUsage::default()
    }
}

#[test]
fn ttl_rates_replace_not_stack_and_snapshot_the_actual_rate() {
    let q = quote(
        usage(),
        &json!({"cache_write_5m":"1.25","cache_write_1h":"2"}),
    )
    .unwrap();
    // (900 + 60*1.25 + 40*2) * $2/1M = 2110 micro.
    assert_eq!(q.amount.as_micros(), 2110);
    let s = serde_json::to_value(q.snapshot).unwrap();
    assert_eq!(s["modality_ratios"]["cache_write_1h"], 2);
    let zero = quote(usage(), &json!({"cache_write_5m":"0","cache_write_1h":"0"})).unwrap();
    assert_eq!(zero.amount.as_micros(), 1800);
}

#[test]
fn missing_ttl_uses_generic_write_rate_without_fabricating_a_split() {
    let u = TokenUsage {
        cache_write_5m_tokens: None,
        cache_write_1h_tokens: None,
        ..usage()
    };
    let q = quote(u, &json!({"cache_write_5m":"1.25","cache_write_1h":"2"})).unwrap();
    assert_eq!(q.amount.as_micros(), 2050);
    assert!(serde_json::to_value(q.snapshot).unwrap()["modality_ratios"].is_null());
    assert_eq!(quote(usage(), &json!({})).unwrap().amount.as_micros(), 2050);
}

#[test]
fn contradictory_or_overlapping_breakdowns_fail_closed() {
    let incomplete = TokenUsage {
        cache_write_1h_tokens: None,
        ..usage()
    };
    assert!(quote(incomplete, &json!({"cache_write_1h":"2"})).is_err());
    let overlapping = TokenUsage {
        cache_write_modalities: Some(CacheModalities {
            image_tokens: 10,
            audio_tokens: 0,
        }),
        ..usage()
    };
    assert!(quote(overlapping, &json!({"cache_write_1h":"2"})).is_err());
}
