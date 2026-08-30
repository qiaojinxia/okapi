//! new-api 公式对拍（M0 验收）。
//!
//! 覆盖：三层倍率 / 缓存 / 按次 / 阶梯 / 免费（模型与分组）/ 截断语义 / 用户覆盖 + 规则栈。
//! quota 对拍语义：`floor(micro / 2)` 等于 new-api `int()` 截断结果（fixtures 均为精确可表示值）。

use okapi_domain::{GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_pricing::{
    CalcContext, GroupEntry, ModelEntry, OverrideEntry, OverrideSpec, PriceBookSource, PricingMode,
    PricingRule, RatioFp, RuleKind, RuleScope, TierTable, book, calculate, ratio::parse_scaled_1e6,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    model: ModelSpec,
    group_ratio: String,
    user_multiplier: String,
    #[serde(default)]
    user_override: Option<ModelSpec>,
    #[serde(default)]
    rules: Vec<RuleSpec>,
    ctx: CtxSpec,
    usage: UsageSpec,
    expect: Expect,
}

#[derive(Deserialize)]
struct ModelSpec {
    mode: String,
    #[serde(default)]
    model_ratio: Option<String>,
    #[serde(default)]
    completion_ratio: Option<String>,
    #[serde(default)]
    cache_ratio: Option<String>,
    #[serde(default)]
    cache_write_ratio: Option<String>,
    #[serde(default)]
    price_usd: Option<String>,
    #[serde(default)]
    tiers: Option<String>,
}

#[derive(Deserialize)]
struct RuleSpec {
    code: String,
    kind: String,
    multiplier: String,
    #[serde(default)]
    min_monthly_tokens: u64,
    #[serde(default)]
    start_minute: u16,
    #[serde(default)]
    end_minute: u16,
}

#[derive(Deserialize)]
struct CtxSpec {
    monthly_tokens: u64,
    local_minute: u16,
    surge: bool,
}

#[derive(Deserialize)]
struct UsageSpec {
    prompt: u32,
    cached: u32,
    #[serde(default)]
    cache_write: u32,
    completion: u32,
}

#[derive(Deserialize)]
struct Expect {
    micro: i64,
    original: i64,
    discount: i64,
    quota: i64,
}

fn ratio(literal: &str) -> RatioFp {
    literal.parse().unwrap()
}

fn pricing_mode(spec: &ModelSpec) -> PricingMode {
    match spec.mode.as_str() {
        "ratio" => PricingMode::Ratio {
            model_ratio: ratio(spec.model_ratio.as_deref().unwrap()),
            completion_ratio: ratio(spec.completion_ratio.as_deref().unwrap_or("1")),
            cache_ratio: ratio(spec.cache_ratio.as_deref().unwrap_or("1")),
            cache_write_ratio: ratio(spec.cache_write_ratio.as_deref().unwrap_or("1")),
        },
        "per_call" => PricingMode::PerCall {
            price: Money::from_micros(
                parse_scaled_1e6(spec.price_usd.as_deref().unwrap()).unwrap(),
            ),
        },
        "tiered" => PricingMode::Tiered {
            completion_ratio: ratio(spec.completion_ratio.as_deref().unwrap_or("1")),
            cache_ratio: ratio(spec.cache_ratio.as_deref().unwrap_or("1")),
            cache_write_ratio: ratio(spec.cache_write_ratio.as_deref().unwrap_or("1")),
            tiers: TierTable::parse(spec.tiers.as_deref().unwrap()).unwrap(),
        },
        other => panic!("unknown pricing mode: {other}"),
    }
}

fn build_rule(spec: &RuleSpec) -> PricingRule {
    let kind = match spec.kind.as_str() {
        "volume" => RuleKind::Volume {
            min_monthly_tokens: spec.min_monthly_tokens,
        },
        "time_based" => RuleKind::TimeBased {
            start_minute: spec.start_minute,
            end_minute: spec.end_minute,
        },
        "discount" => RuleKind::Discount,
        "surge" => RuleKind::Surge,
        other => panic!("unknown rule kind: {other}"),
    };
    PricingRule {
        code: spec.code.clone(),
        kind,
        multiplier: ratio(&spec.multiplier),
        scope: RuleScope::default(),
        priority: 0,
        valid_from: None,
        valid_to: None,
    }
}

#[test]
fn newapi_parity_fixtures() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/newapi_parity.json")).unwrap();
    assert!(cases.len() >= 12, "fixture 覆盖面不足");

    for case in cases {
        let user = UserId::new(1);
        let model_code = ModelCode::from("m");
        let group_code = GroupCode::from("g");

        let mut overrides = Vec::new();
        if let Some(spec) = &case.user_override {
            overrides.push(OverrideEntry {
                user,
                model: model_code.clone(),
                spec: OverrideSpec::Ratio(pricing_mode(spec)),
            });
        }

        let source = PriceBookSource {
            epoch: 1042,
            models: vec![ModelEntry {
                model: model_code.clone(),
                pricing: pricing_mode(&case.model),
                tier_ratios: Vec::new(),
            }],
            groups: vec![GroupEntry {
                group: group_code.clone(),
                ratio: ratio(&case.group_ratio),
            }],
            overrides,
            rules: case.rules.iter().map(build_rule).collect(),
        };
        let book = book::compile(source).unwrap();

        let ctx = CalcContext {
            user,
            model: model_code,
            group: group_code,
            user_multiplier: ratio(&case.user_multiplier),
            monthly_tokens: case.ctx.monthly_tokens,
            local_minute_of_day: case.ctx.local_minute,
            now_unix: 1_756_500_000,
            surge_active: case.ctx.surge,
            service_tier: None,
        };
        let usage = TokenUsage {
            prompt_tokens: case.usage.prompt,
            cached_tokens: case.usage.cached,
            cache_write_tokens: case.usage.cache_write,
            completion_tokens: case.usage.completion,
            reasoning_tokens: 0,
        };

        let quote = calculate(&book, &ctx, usage).unwrap();
        assert_eq!(
            quote.amount.as_micros(),
            case.expect.micro,
            "{}: amount",
            case.name
        );
        assert_eq!(
            quote.original.as_micros(),
            case.expect.original,
            "{}: original",
            case.name
        );
        assert_eq!(
            quote.discount.as_micros(),
            case.expect.discount,
            "{}: discount",
            case.name
        );
        assert_eq!(
            quote.amount.to_quota_floor(),
            case.expect.quota,
            "{}: quota 视图（new-api int 截断对拍）",
            case.name
        );
        assert_eq!(quote.snapshot.epoch, 1042, "{}: snapshot epoch", case.name);

        if case.name == "override_and_stack" {
            let codes: Vec<&str> = quote
                .snapshot
                .rules
                .iter()
                .map(|r| r.code.as_str())
                .collect();
            assert_eq!(
                codes,
                ["vol", "promo"],
                "规则施加顺序：volume 先于 discount"
            );
        }
    }
}

/// 快照 JSON 形状对齐 DESIGN §3.4：倍率为精确数字、规则链完整、含最终单价。
#[test]
fn snapshot_json_shape_matches_design() {
    let model_code = ModelCode::from("gpt-4o");
    let group_code = GroupCode::from("vip");
    let source = PriceBookSource {
        epoch: 1042,
        models: vec![ModelEntry {
            model: model_code.clone(),
            pricing: PricingMode::Ratio {
                model_ratio: ratio("1.25"),
                completion_ratio: ratio("4"),
                cache_ratio: ratio("0.5"),
                cache_write_ratio: ratio("1.25"),
            },
            tier_ratios: Vec::new(),
        }],
        groups: vec![GroupEntry {
            group: group_code.clone(),
            ratio: ratio("0.9"),
        }],
        overrides: Vec::new(),
        rules: vec![PricingRule {
            code: "night-discount".to_owned(),
            kind: RuleKind::TimeBased {
                start_minute: 1320,
                end_minute: 360,
            },
            multiplier: ratio("0.8"),
            scope: RuleScope::default(),
            priority: 0,
            valid_from: None,
            valid_to: None,
        }],
    };
    let book = book::compile(source).unwrap();
    let ctx = CalcContext {
        user: UserId::new(7),
        model: model_code,
        group: group_code,
        user_multiplier: ratio("1"),
        monthly_tokens: 0,
        local_minute_of_day: 1380,
        now_unix: 0,
        surge_active: false,
        service_tier: None,
    };
    let usage = TokenUsage {
        prompt_tokens: 1000,
        cached_tokens: 0,
        cache_write_tokens: 0,
        completion_tokens: 500,
        reasoning_tokens: 0,
    };
    let quote = calculate(&book, &ctx, usage).unwrap();
    let json = serde_json::to_string(&quote.snapshot).unwrap();

    assert!(json.contains("\"epoch\":1042"), "{json}");
    assert!(json.contains("\"model_ratio\":1.25"), "{json}");
    assert!(json.contains("\"completion_ratio\":4"), "{json}");
    assert!(json.contains("\"cache_ratio\":0.5"), "{json}");
    assert!(json.contains("\"group\":\"vip\""), "{json}");
    assert!(json.contains("\"group_ratio\":0.9"), "{json}");
    assert!(json.contains("\"type\":\"time_based\""), "{json}");
    assert!(json.contains("\"multiplier\":0.8"), "{json}");
    assert!(
        json.contains("\"final_unit_price_input_per_1m_usd\":1.8"),
        "{json}"
    );
    assert!(
        !json.contains("cache_write_ratio"),
        "无缓存写入时不得出现该字段（避免 OpenAI 账单带无意义轴）：{json}"
    );
}

/// Anthropic prompt caching 四段计费对拍（DESIGN §3.2）。
///
/// 官方定价（claude-3-5-sonnet）：input $3/1M、output $15/1M、
/// cache read $0.3/1M（0.1×）、cache write@5m $3.75/1M（1.25×）。
/// 本用例同时锁定"缓存写入独立成段"的**金额差**——若退回旧实现（写入混进常规输入段
/// 按 1.0× 计），本站会对该请求少收 2250 micro（9.6%）。
#[test]
fn anthropic_cache_write_is_billed_as_separate_segment() {
    let model_code = ModelCode::from("claude-3-5-sonnet");
    let group_code = GroupCode::from("default");
    let build = |cache_write_ratio: &str| {
        let source = PriceBookSource {
            epoch: 7,
            models: vec![ModelEntry {
                model: model_code.clone(),
                pricing: PricingMode::Ratio {
                    model_ratio: ratio("1.5"),
                    completion_ratio: ratio("5"),
                    cache_ratio: ratio("0.1"),
                    cache_write_ratio: ratio(cache_write_ratio),
                },
                tier_ratios: Vec::new(),
            }],
            groups: vec![GroupEntry {
                group: group_code.clone(),
                ratio: ratio("1"),
            }],
            overrides: Vec::new(),
            rules: Vec::new(),
        };
        book::compile(source).unwrap()
    };
    let ctx = CalcContext {
        user: UserId::new(1),
        model: model_code.clone(),
        group: group_code.clone(),
        user_multiplier: ratio("1"),
        monthly_tokens: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        surge_active: false,
        service_tier: None,
    };
    // prompt 10000 = 常规 1000 + 缓存读 6000 + 缓存写 3000
    let usage = TokenUsage {
        prompt_tokens: 10_000,
        cached_tokens: 6_000,
        cache_write_tokens: 3_000,
        completion_tokens: 500,
        reasoning_tokens: 0,
    };
    assert!(usage.validate().is_ok());

    // 逐段核算：1000×$3/1M + 6000×$0.3/1M + 3000×$3.75/1M + 500×$15/1M
    //         = 3000 + 1800 + 11250 + 7500 = 23550 micro
    let quote = calculate(&build("1.25"), &ctx, usage).unwrap();
    assert_eq!(quote.amount.as_micros(), 23_550, "四段合计");
    assert_eq!(
        quote.snapshot.cache_write_ratio.map(|r| r.to_string()),
        Some("1.25".to_owned()),
        "缓存写入倍率必须入快照（账单可解释）"
    );

    // 旧行为（写入按常规输入 1.0× 计）：4000×$3/1M + 1800 + 7500 = 21300
    let legacy = calculate(&build("1"), &ctx, usage).unwrap();
    assert_eq!(legacy.amount.as_micros(), 21_300);
    assert_eq!(
        quote.amount.as_micros() - legacy.amount.as_micros(),
        2_250,
        "缺失缓存写入轴 = 每笔少收 2250 micro"
    );
}
