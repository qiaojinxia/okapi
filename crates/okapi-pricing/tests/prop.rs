//! 性质测试（M0 验收）：有界输入下不失败、免费短路、金额恒等式、单调性。

use okapi_domain::{GroupCode, ModelCode, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, ModelEntry, PriceBook, PriceBookSource, PricingMode, PricingRule,
    RatioFp, RuleKind, RuleScope, book, calculate,
};
use proptest::prelude::*;

fn fp(scaled: i64) -> RatioFp {
    RatioFp::from_scaled(scaled).unwrap()
}

fn build_book(
    model_fp: i64,
    completion_fp: i64,
    cache_fp: i64,
    group_fp: i64,
    rule_fp: Option<i64>,
) -> PriceBook {
    build_book_with_cache_write(
        model_fp,
        completion_fp,
        cache_fp,
        1_000_000,
        group_fp,
        rule_fp,
    )
}

fn build_book_with_cache_write(
    model_fp: i64,
    completion_fp: i64,
    cache_fp: i64,
    cache_write_fp: i64,
    group_fp: i64,
    rule_fp: Option<i64>,
) -> PriceBook {
    let mut rules = Vec::new();
    if let Some(mult) = rule_fp {
        rules.push(PricingRule {
            code: "r".to_owned(),
            kind: RuleKind::Discount,
            multiplier: fp(mult),
            scope: RuleScope::default(),
            priority: 0,
            stacking: okapi_pricing::Stacking::Stackable,
            valid_from: None,
            valid_to: None,
        });
    }
    book::compile(PriceBookSource {
        epoch: 1,
        models: vec![ModelEntry {
            model: ModelCode::from("m"),
            pricing: PricingMode::Ratio {
                model_ratio: fp(model_fp),
                completion_ratio: fp(completion_fp),
                cache_ratio: fp(cache_fp),
                cache_write_ratio: fp(cache_write_fp),
                audio_ratio: RatioFp::ONE,
                audio_completion_ratio: RatioFp::ONE,
                image_ratio: RatioFp::ONE,
            },
            tier_ratios: Vec::new(),
        }],
        groups: vec![GroupEntry {
            group: GroupCode::from("g"),
            ratio: fp(group_fp),
        }],
        overrides: Vec::new(),
        rules,
    })
    .unwrap()
}

fn ctx(user_fp: i64) -> CalcContext {
    CalcContext {
        user: UserId::new(1),
        model: ModelCode::from("m"),
        group: GroupCode::from("g"),
        user_multiplier: fp(user_fp),
        monthly_tokens: 0,
        monthly_spend_micro: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        surge_active: false,
        service_tier: None,
    }
}

fn usage_of(prompt: u32, cached: u32, completion: u32) -> TokenUsage {
    TokenUsage {
        prompt_tokens: prompt,
        cached_tokens: cached,
        cache_write_tokens: 0,
        audio_prompt_tokens: 0,
        image_prompt_tokens: 0,
        completion_tokens: completion,
        audio_completion_tokens: 0,
        reasoning_tokens: 0,
    }
}

fn usage_split(prompt: u32, cached: u32, cache_write: u32, completion: u32) -> TokenUsage {
    TokenUsage {
        prompt_tokens: prompt,
        cached_tokens: cached,
        cache_write_tokens: cache_write,
        audio_prompt_tokens: 0,
        image_prompt_tokens: 0,
        completion_tokens: completion,
        audio_completion_tokens: 0,
        reasoning_tokens: 0,
    }
}

proptest! {
    /// 有界输入下：计算不失败、金额非负、discount == original − amount。
    #[test]
    fn calculation_holds_invariants(
        model in 0i64..=1_000_000_000,
        completion in 0i64..=50_000_000,
        cache in 0i64..=1_000_000,
        group in 0i64..=5_000_000,
        user in 0i64..=5_000_000,
        rule in proptest::option::of(0i64..=5_000_000),
        prompt in 0u32..=10_000_000,
        cached_pct in 0u32..=100,
        completion_tokens in 0u32..=10_000_000,
    ) {
        let cached = u32::try_from(u64::from(prompt) * u64::from(cached_pct) / 100).unwrap();
        let book = build_book(model, completion, cache, group, rule);
        let quote = calculate(&book, &ctx(user), usage_of(prompt, cached, completion_tokens));
        prop_assert!(quote.is_ok(), "err={:?}", quote.err());
        let quote = quote.unwrap();
        prop_assert!(quote.amount.as_micros() >= 0);
        prop_assert!(quote.original.as_micros() >= 0);
        prop_assert_eq!(quote.original.checked_sub(quote.amount), Some(quote.discount));
    }

    /// 免费短路：模型倍率或分组倍率为 0 → 全部金额为 0。
    #[test]
    fn zero_ratio_means_free(
        completion in 0i64..=50_000_000,
        group in 0i64..=5_000_000,
        user in 0i64..=5_000_000,
        prompt in 0u32..=10_000_000,
        completion_tokens in 0u32..=10_000_000,
        zero_group in proptest::bool::ANY,
    ) {
        let (model_fp, group_fp) = if zero_group { (1_250_000, 0) } else { (0, group) };
        let book = build_book(model_fp, completion, 1_000_000, group_fp, None);
        let quote = calculate(&book, &ctx(user), usage_of(prompt, 0, completion_tokens)).unwrap();
        prop_assert_eq!(quote.amount.as_micros(), 0);
        prop_assert_eq!(quote.original.as_micros(), 0);
        prop_assert_eq!(quote.discount.as_micros(), 0);
    }

    /// 无个人倍率、无规则时：实付 == 标价。
    #[test]
    fn amount_equals_original_without_modifiers(
        model in 0i64..=1_000_000_000,
        completion in 0i64..=50_000_000,
        group in 0i64..=5_000_000,
        prompt in 0u32..=10_000_000,
        completion_tokens in 0u32..=10_000_000,
    ) {
        let book = build_book(model, completion, 1_000_000, group, None);
        let quote = calculate(&book, &ctx(1_000_000), usage_of(prompt, 0, completion_tokens)).unwrap();
        prop_assert_eq!(quote.amount, quote.original);
        prop_assert_eq!(quote.discount.as_micros(), 0);
    }

    /// prompt 三段（常规/缓存读/缓存写）在三轴倍率均为 1 时完全等价——
    /// 无论怎么切分都不改变金额。这条性质专防"某一段被漏乘或漏加"的计费缺陷。
    #[test]
    fn prompt_segments_are_conservative_when_ratios_are_one(
        model in 0i64..=1_000_000_000,
        completion in 0i64..=50_000_000,
        group in 0i64..=5_000_000,
        user in 0i64..=5_000_000,
        prompt in 0u32..=1_000_000,
        cached_pct in 0u32..=50,
        write_pct in 0u32..=50,
        completion_tokens in 0u32..=1_000_000,
    ) {
        let cached = u32::try_from(u64::from(prompt) * u64::from(cached_pct) / 100).unwrap();
        let cache_write = u32::try_from(u64::from(prompt) * u64::from(write_pct) / 100).unwrap();
        let book = build_book_with_cache_write(model, completion, 1_000_000, 1_000_000, group, None);
        let split = calculate(&book, &ctx(user), usage_split(prompt, cached, cache_write, completion_tokens)).unwrap();
        let flat = calculate(&book, &ctx(user), usage_of(prompt, 0, completion_tokens)).unwrap();
        prop_assert_eq!(split.amount, flat.amount, "三轴为 1 时分段不得改变金额");
    }

    /// 缓存写入段：倍率越高金额越高（加价语义方向正确，不会把加价做成打折）。
    #[test]
    fn cache_write_ratio_is_monotonic(
        model in 1i64..=1_000_000_000,
        group in 1i64..=5_000_000,
        prompt in 1u32..=1_000_000,
        write_pct in 1u32..=100,
        low in 0i64..=1_000_000,
        delta in 1i64..=4_000_000,
    ) {
        let cache_write = u32::try_from(u64::from(prompt) * u64::from(write_pct) / 100).unwrap();
        let usage = usage_split(prompt, 0, cache_write, 0);
        let cheap = build_book_with_cache_write(model, 1_000_000, 1_000_000, low, group, None);
        let pricey = build_book_with_cache_write(model, 1_000_000, 1_000_000, low + delta, group, None);
        let a = calculate(&cheap, &ctx(1_000_000), usage).unwrap();
        let b = calculate(&pricey, &ctx(1_000_000), usage).unwrap();
        prop_assert!(b.amount >= a.amount);
    }

    /// completion tokens 单调不减 → 金额单调不减。
    #[test]
    fn monotonic_in_completion_tokens(
        model in 0i64..=1_000_000_000,
        completion in 0i64..=50_000_000,
        group in 0i64..=5_000_000,
        user in 0i64..=5_000_000,
        prompt in 0u32..=1_000_000,
        completion_tokens in 0u32..=1_000_000,
        delta in 1u32..=100_000,
    ) {
        let book = build_book(model, completion, 1_000_000, group, None);
        let base = calculate(&book, &ctx(user), usage_of(prompt, 0, completion_tokens)).unwrap();
        let more = calculate(&book, &ctx(user), usage_of(prompt, 0, completion_tokens + delta)).unwrap();
        prop_assert!(more.amount >= base.amount);
    }
}
