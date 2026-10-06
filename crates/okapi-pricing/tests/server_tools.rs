use okapi_domain::{AnthropicToolUsage, Money, ServerToolUsage, TokenUsage, UserId};
use okapi_pricing::{
    AnthropicToolPrices, AnthropicToolScope, CalcContext, GroupEntry, ModelEntry, PriceBook,
    PriceBookSource, PricingMode, PricingRule, RatioFp, RuleKind, RuleScope, ServerToolPrices,
    Stacking, ToolPrice, book, calculate, calculate_with_server_tool_scope,
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn fp(scaled: i64) -> RatioFp {
    RatioFp::from_scaled(scaled).unwrap()
}
fn ctx(user: i64) -> CalcContext {
    CalcContext {
        user: UserId::new(1),
        model: "m".into(),
        group: "g".into(),
        user_multiplier: fp(user),
        monthly_tokens: 0,
        monthly_spend_micro: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        utc_offset_seconds: 0,
        surge_active: false,
        service_tier: Some("priority".into()),
    }
}
fn token_mode() -> PricingMode {
    PricingMode::Ratio {
        model_ratio: fp(7_000_000),
        completion_ratio: fp(4_000_000),
        cache_ratio: fp(250_000),
        cache_write_ratio: fp(2_000_000),
        audio_ratio: fp(16_000_000),
        audio_completion_ratio: fp(2_000_000),
        image_ratio: fp(3_000_000),
        modality_ratios: okapi_pricing::ModalityRatios::default(),
    }
}
fn profile(price: i64) -> ServerToolPrices {
    ServerToolPrices::AnthropicServerToolUseV1(AnthropicToolPrices {
        web_search: ToolPrice::Additional {
            price_per_request_micro: Money::from_micros(price),
        },
        web_fetch: ToolPrice::Included {},
    })
}
fn pricebook(mode: PricingMode, group: i64, rule: i64, price: Option<i64>) -> PriceBook {
    let book = book::compile(PriceBookSource {
        epoch: 71,
        models: vec![ModelEntry {
            model: "m".into(),
            pricing: mode,
            tier_ratios: vec![("priority".into(), fp(2_000_000))],
        }],
        groups: vec![GroupEntry {
            group: "g".into(),
            ratio: fp(group),
        }],
        overrides: vec![],
        rules: vec![PricingRule {
            code: "discount".into(),
            kind: RuleKind::Discount,
            multiplier: fp(rule),
            scope: RuleScope::default(),
            priority: 0,
            stacking: Stacking::Stackable,
            valid_from: None,
            valid_to: None,
        }],
    })
    .unwrap();
    match price {
        None => book,
        Some(price) => book
            .with_server_tool_prices([("m".into(), profile(price))])
            .unwrap(),
    }
}
fn usage(search: Option<u32>, fetch: Option<u32>) -> TokenUsage {
    TokenUsage {
        server_tool_usage: Some(ServerToolUsage::Anthropic(AnthropicToolUsage {
            web_search_requests: search,
            web_fetch_requests: fetch,
            code_execution_requests: None,
        })),
        ..Default::default()
    }
}

#[test]
fn exact_independent_fee_fixtures() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/server_tool_parity.json")).unwrap();
    for c in cases {
        let n = |key: &str| c[key].as_i64().unwrap();
        let count = |key: &str| c[key].as_u64().map(|n| u32::try_from(n).unwrap());
        let source = usage(count("search"), count("fetch"));
        let book = pricebook(token_mode(), n("group"), n("rule"), Some(n("price")));
        let context = ctx(n("user"));
        let scope = c
            .get("scope")
            .map(|value| serde_json::from_value::<AnthropicToolScope>(value.clone()).unwrap());
        let quote = match scope {
            Some(scope) => calculate_with_server_tool_scope(&book, &context, source, scope),
            None => calculate(&book, &context, source),
        }
        .unwrap();
        assert_eq!(quote.snapshot.server_tool_fees[0].quantity, count("search"));
        assert_eq!(quote.snapshot.server_tool_fees[1].quantity, count("fetch"));
        assert_eq!(
            quote.snapshot.server_tool_fees[0].requested,
            scope.map(|s| s.web_search)
        );
        assert_eq!(
            (
                quote.list_price.as_micros(),
                quote.original.as_micros(),
                quote.amount.as_micros()
            ),
            (n("list"), n("original"), n("amount")),
            "{}",
            c["name"]
        );
        assert_eq!(quote.discount.as_micros(), n("original") - n("amount"));
        assert_eq!(quote.snapshot.epoch, 71);
        assert_eq!(quote.snapshot.server_tool_fees.len(), 2);
        assert_eq!(
            quote.snapshot.server_tool_fees[1].amount_micro,
            Some(Money::ZERO)
        );
    }
}

#[test]
fn execution_count_does_not_create_a_duration_price_or_change_supported_fees() {
    let book = pricebook(token_mode(), 1_500_000, 900_000, Some(10_000));
    let source = usage(Some(2), Some(3));
    let expected = calculate(&book, &ctx(800_000), source).unwrap();
    for count in [None, Some(0), Some(1), Some(2_147_483_647)] {
        let quote = calculate(
            &book,
            &ctx(800_000),
            TokenUsage {
                server_tool_usage: Some(ServerToolUsage::Anthropic(AnthropicToolUsage {
                    web_search_requests: Some(2),
                    web_fetch_requests: Some(3),
                    code_execution_requests: count,
                })),
                ..source
            },
        )
        .unwrap();
        assert_eq!(
            (
                quote.amount,
                quote.original,
                quote.discount,
                quote.list_price
            ),
            (
                expected.amount,
                expected.original,
                expected.discount,
                expected.list_price
            )
        );
        assert_eq!(
            serde_json::to_value(&quote.snapshot).unwrap(),
            serde_json::to_value(&expected.snapshot).unwrap()
        );
    }
}

#[test]
fn fees_do_not_follow_model_or_tier_prices_and_preserve_plain_token_rounding() {
    for mode in [
        token_mode(),
        PricingMode::PerCall {
            price: Money::from_micros(123),
        },
    ] {
        let book = pricebook(mode, 1_500_000, 900_000, Some(10_000));
        let base = TokenUsage {
            prompt_tokens: 17,
            completion_tokens: 3,
            ..Default::default()
        };
        let plain = calculate(&book, &ctx(800_000), base).unwrap();
        let quote = calculate(
            &book,
            &ctx(800_000),
            TokenUsage {
                server_tool_usage: usage(Some(3), Some(2)).server_tool_usage,
                ..base
            },
        )
        .unwrap();
        assert_eq!(
            quote.list_price.as_micros() - plain.list_price.as_micros(),
            30_000
        );
        assert_eq!(
            quote.original.as_micros() - plain.original.as_micros(),
            45_000
        );
        assert_eq!(quote.amount.as_micros() - plain.amount.as_micros(), 32_400);
        assert_eq!(quote.snapshot.rules.len(), plain.snapshot.rules.len());
        assert_eq!(
            quote.snapshot.final_unit_price_input_per_1m_usd,
            plain.snapshot.final_unit_price_input_per_1m_usd
        );
    }
}

#[test]
fn strict_config_rejects_unknown_shapes_units_and_negative_or_fractional_prices() {
    let valid = json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":{"billing":"additional","price_per_request_micro":10000},"web_fetch":{"billing":"included"}});
    assert_eq!(
        ServerToolPrices::parse(&valid).unwrap(),
        Some(profile(10000))
    );
    assert_eq!(ServerToolPrices::parse(&json!({})).unwrap(), None);
    for invalid in [
        Value::Null,
        json!([]),
        json!({"usage_contract":"other"}),
        json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":{"billing":"included"}}),
        json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":{"billing":"additional","price_per_request_micro":-1},"web_fetch":{"billing":"included"}}),
    ] {
        assert!(ServerToolPrices::parse(&invalid).is_err(), "{invalid}");
    }
    for (path, value) in [
        ("price_per_request_micro", json!("10000")),
        (
            "price_per_request_micro",
            serde_json::from_str("1.5").unwrap(),
        ),
        ("price_per_request_micro", json!(null)),
        ("unit", json!("token")),
    ] {
        let mut invalid = valid.clone();
        invalid["web_search"][path] = value;
        assert!(ServerToolPrices::parse(&invalid).is_err(), "{invalid}");
    }
    let mut invalid = valid.clone();
    invalid["unknown"] = json!(true);
    assert!(ServerToolPrices::parse(&invalid).is_err(), "{invalid}");
    let mut invalid = valid;
    invalid["web_fetch"]["price_per_request_micro"] = json!(10000);
    assert!(ServerToolPrices::parse(&invalid).is_err(), "{invalid}");
}

#[test]
fn unknown_price_and_quantity_are_auditable_and_not_explicit_free() {
    let legacy = pricebook(token_mode(), 1_000_000, 1_000_000, None);
    let quote = calculate(&legacy, &ctx(1_000_000), usage(Some(2), None)).unwrap();
    let rows = serde_json::to_value(quote.snapshot).unwrap();
    assert_eq!(rows["server_tool_fees"][0]["quantity"], 2);
    assert!(rows["server_tool_fees"][0]["pricing"].is_null());
    assert!(rows["server_tool_fees"][0]["amount_micro"].is_null());
    assert!(rows["server_tool_fees"][1]["quantity"].is_null());
    let free = pricebook(token_mode(), 1_000_000, 1_000_000, Some(0));
    let quote = calculate(&free, &ctx(1_000_000), usage(Some(2), None)).unwrap();
    assert_eq!(
        quote.snapshot.server_tool_fees[0].amount_micro,
        Some(Money::ZERO)
    );
    assert!(quote.snapshot.server_tool_fees[1].amount_micro.is_none());
    let paid = pricebook(token_mode(), 1_000_000, 1_000_000, Some(10_000));
    assert!(calculate(&paid, &ctx(1_000_000), usage(None, Some(2))).is_err());
    assert!(calculate(&paid, &ctx(1_000_000), TokenUsage::default()).is_ok());
}

#[test]
fn overflow_and_invalid_profiles_fail_closed() {
    let book = pricebook(token_mode(), 1_000_000, 1_000_000, Some(i64::MAX));
    assert!(calculate(&book, &ctx(1_000_000), usage(Some(2), Some(0))).is_err());
    let mut context = ctx(1_000_000);
    context.service_tier = None;
    let book = pricebook(
        PricingMode::PerCall {
            price: Money::from_micros(i64::MAX),
        },
        1_000_000,
        1_000_000,
        Some(1),
    );
    assert!(calculate(&book, &context, usage(Some(1), Some(0))).is_err());
    let book = pricebook(token_mode(), 1_000_000, 1_000_000, None);
    assert!(
        book.clone()
            .with_server_tool_prices([("m".into(), profile(-1))])
            .is_err()
    );
    assert!(
        book.with_server_tool_prices([("m".into(), profile(1)), ("m".into(), profile(2))])
            .is_err()
    );
}

proptest! {
    #[test]
    fn fee_components_match_independent_integer_formula(
        count in 0_u32..100_000, price in 0_i64..1_000_000, group in 0_i64..3_000_000,
        user in 0_i64..3_000_000, rule in 0_i64..3_000_000, prompt in 0_u32..100_000,
    ) {
        let book=pricebook(token_mode(),group,rule,Some(price));
        let base=TokenUsage {prompt_tokens:prompt,..Default::default()};
        let plain=calculate(&book,&ctx(user),base).unwrap();
        let quote=calculate(&book,&ctx(user),TokenUsage {server_tool_usage:usage(Some(count),Some(0)).server_tool_usage,..base}).unwrap();
        let list=i128::from(count)*i128::from(price);
        let scaled=list*1_000_000*i128::from(group)/1_000_000;
        let original=i64::try_from(scaled/1_000_000).unwrap();
        let amount=i64::try_from((scaled*i128::from(user)/1_000_000)*i128::from(rule)/1_000_000/1_000_000).unwrap();
        prop_assert_eq!(quote.list_price.as_micros()-plain.list_price.as_micros(),i64::try_from(list).unwrap());
        prop_assert_eq!(quote.original.as_micros()-plain.original.as_micros(),original);
        prop_assert_eq!(quote.amount.as_micros()-plain.amount.as_micros(),amount);
        prop_assert_eq!(quote.discount,quote.original.checked_sub(quote.amount).unwrap());
    }
}

#[test]
fn scope_never_authorizes_nonzero_unrequested_usage_or_fills_missing_paid_counts() {
    let book = pricebook(token_mode(), 1_000_000, 1_000_000, Some(10000));
    let context = ctx(1_000_000);
    let fetch_only = AnthropicToolScope {
        web_search: false,
        web_fetch: true,
    };
    assert!(
        calculate_with_server_tool_scope(&book, &context, usage(Some(1), Some(2)), fetch_only)
            .is_err()
    );
    let search_only = AnthropicToolScope {
        web_search: true,
        web_fetch: false,
    };
    for source in [TokenUsage::default(), usage(None, Some(0))] {
        assert!(calculate_with_server_tool_scope(&book, &context, source, search_only).is_err());
    }
    let unpriced = pricebook(token_mode(), 1_000_000, 1_000_000, None);
    assert!(
        calculate_with_server_tool_scope(&unpriced, &context, usage(Some(1), Some(2)), fetch_only)
            .is_err()
    );
    let quote =
        calculate_with_server_tool_scope(&unpriced, &context, usage(None, Some(2)), fetch_only)
            .unwrap();
    let rows = serde_json::to_value(quote.snapshot).unwrap();
    assert_eq!(rows["server_tool_fees"][0]["requested"], false);
    assert!(rows["server_tool_fees"][0]["quantity"].is_null());
    assert!(rows["server_tool_fees"][0]["pricing"].is_null());
    assert_eq!(rows["server_tool_fees"][0]["amount_micro"], 0);
    assert!(rows["server_tool_fees"][1]["amount_micro"].is_null());
}

proptest! {
    #[test]
    fn unrequested_search_price_cannot_change_any_money_or_manufacture_quantity(
        price in 0_i64..=i64::MAX, group in 0_i64..3_000_000,
        user in 0_i64..3_000_000, rule in 0_i64..3_000_000,
        count in 0_u32..100_000, prompt in 0_u32..100_000,
    ) {
        let book=pricebook(token_mode(),group,rule,Some(price));
        let base=TokenUsage {prompt_tokens:prompt,..Default::default()};
        let context=ctx(user);
        let plain=calculate(&book,&context,base).unwrap();
        let source=TokenUsage {server_tool_usage:usage(None,Some(count)).server_tool_usage,..base};
        let quote=calculate_with_server_tool_scope(&book,&context,source,AnthropicToolScope {web_search:false,web_fetch:true}).unwrap();
        prop_assert_eq!((quote.amount,quote.original,quote.discount,quote.list_price),(plain.amount,plain.original,plain.discount,plain.list_price));
        prop_assert_eq!(quote.snapshot.server_tool_fees[0].quantity,None);
        prop_assert_eq!(quote.snapshot.server_tool_fees[1].quantity,Some(count));
        prop_assert_eq!(quote.snapshot.server_tool_fees[0].requested,Some(false));
        prop_assert_eq!(source.total_raw(),base.total_raw());
    }
}
