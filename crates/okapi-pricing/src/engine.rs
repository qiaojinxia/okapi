//! 计费引擎：PriceBook 上的纯整数乘加，请求路径零 IO（DESIGN §3.2 / §3.4）。
//!
//! 取整语义：每一步乘法后对 scale 做 floor（`div_euclid`），最终 micro 亦 floor——
//! 与 new-api `int()` 截断在精确可表示场景下一致（parity fixtures 验证）。

use crate::book::{BASE_PRICE_PER_1M_MICRO, PriceBook};
use crate::error::PricingError;
use crate::model::PricingMode;
use crate::ratio::{RATIO_SCALE, RatioFp};
use crate::snapshot::{AppliedRule, PricingSnapshot};
use okapi_domain::{GroupCode, ModelCode, Money, TokenUsage, UserId};

/// 每 token 基准价（micro）：$2/1M。
const BASE_MICRO_PER_TOKEN: i128 = 2;

/// 请求上下文：运行期动态因素全部由调用方注入，engine 不做任何 IO。
#[derive(Debug, Clone)]
pub struct CalcContext {
    pub user: UserId,
    pub model: ModelCode,
    /// 用户的生效定价分组（多组取优先级最高，M2 起由 console/gateway 解析）。
    pub group: GroupCode,
    /// users.price_multiplier。
    pub user_multiplier: RatioFp,
    /// 用户当月累计 tokens（Redis KPI 计数，volume 规则输入）。
    pub monthly_tokens: u64,
    /// 站点本地时区分钟数 0..1440（time_based 规则输入）。
    pub local_minute_of_day: u16,
    /// unix 秒（规则生效窗口判定）。
    pub now_unix: i64,
    /// 网关本地负载判定（surge 规则输入）。
    pub surge_active: bool,
    /// service_tier 结算档位（None = default/未启用；只降不升选择在 gateway 侧完成，
    /// DESIGN §3-4.5）。
    pub service_tier: Option<String>,
}

/// 报价结果：四金额语义对齐 IMPLEMENTATION §5.1/§5.2。
#[derive(Debug, Clone)]
pub struct Quote {
    /// 实付（标价 × 个人倍率 × 规则栈）。
    pub amount: Money,
    /// 标价（模型 × 补全 × 缓存 × 分组；个人倍率与规则不进标价）。
    pub original: Money,
    /// original − amount（负值 = surge 加价）。
    pub discount: Money,
    /// 每笔账可解释的完整快照。
    pub snapshot: PricingSnapshot,
}

/// 一次计费用到的 token 侧倍率轴（DESIGN §3.2）。
struct RatioSet {
    model: RatioFp,
    completion: RatioFp,
    /// 缓存读取（命中折扣）。
    cache: RatioFp,
    /// 缓存写入（创建加价）。
    cache_write: RatioFp,
    /// 音频输入（相对文本的倍数）。
    audio: RatioFp,
    /// 音频输出（叠乘在 audio 之上）。
    audio_completion: RatioFp,
    /// 图片输入（相对文本的倍数）。
    image: RatioFp,
}

/// 单步乘法：value × ratio / SCALE（floor）。
fn step(value: i128, ratio: RatioFp) -> Result<i128, PricingError> {
    value
        .checked_mul(i128::from(ratio.as_scaled()))
        .map(|v| v.div_euclid(i128::from(RATIO_SCALE)))
        .ok_or(PricingError::Overflow)
}

/// token·scale 空间 → micro-USD（× 每 token 基准价，floor）。
fn micro_from_token_scaled(value: i128) -> Result<Money, PricingError> {
    let micros = value
        .checked_mul(BASE_MICRO_PER_TOKEN)
        .ok_or(PricingError::Overflow)?
        .div_euclid(i128::from(RATIO_SCALE));
    i64::try_from(micros)
        .map(Money::from_micros)
        .map_err(|_| PricingError::Overflow)
}

/// micro·scale 空间 → micro-USD（floor）。
fn micro_from_money_scaled(value: i128) -> Result<Money, PricingError> {
    let micros = value.div_euclid(i128::from(RATIO_SCALE));
    i64::try_from(micros)
        .map(Money::from_micros)
        .map_err(|_| PricingError::Overflow)
}

/// $/1M 单价 → 等效模型倍率（阶梯计费代入 model_ratio 位置）。
fn ratio_from_price_per_1m(price: Money) -> Result<RatioFp, PricingError> {
    let scaled = i128::from(price.as_micros())
        .checked_mul(i128::from(RATIO_SCALE))
        .map(|v| v.div_euclid(i128::from(BASE_PRICE_PER_1M_MICRO)))
        .ok_or(PricingError::Overflow)?;
    i64::try_from(scaled)
        .ok()
        .and_then(RatioFp::from_scaled)
        .ok_or(PricingError::Overflow)
}

/// 计算一笔请求的报价。
pub fn calculate(
    book: &PriceBook,
    ctx: &CalcContext,
    usage: TokenUsage,
) -> Result<Quote, PricingError> {
    usage.validate()?;
    let resolved = book.resolve(ctx.user, &ctx.model, &ctx.group)?;
    let group_ratio = resolved.group_ratio;

    // service_tier 档位修饰（DESIGN §3-4.5）：有效 model_ratio ×= tier_ratio；
    // 未配置模型/档位名 → 1.0（无修饰、快照不记）。
    let tier: Option<(String, RatioFp)> = ctx
        .service_tier
        .as_deref()
        .and_then(|t| book.tier_ratio(&ctx.model, t).map(|r| (t.to_owned(), r)));
    let tier_r = tier.as_ref().map_or(RatioFp::ONE, |(_, r)| *r);

    match resolved.pricing {
        PricingMode::Ratio {
            model_ratio,
            completion_ratio,
            cache_ratio,
            cache_write_ratio,
            audio_ratio,
            audio_completion_ratio,
            image_ratio,
        } => {
            let set = RatioSet {
                model: mul_ratio(*model_ratio, tier_r)?,
                completion: *completion_ratio,
                cache: *cache_ratio,
                cache_write: *cache_write_ratio,
                audio: *audio_ratio,
                audio_completion: *audio_completion_ratio,
                image: *image_ratio,
            };
            calc_tokens(book, ctx, usage, &set, group_ratio, "ratio", tier.as_ref())
        }
        PricingMode::Tiered {
            completion_ratio,
            cache_ratio,
            cache_write_ratio,
            audio_ratio,
            audio_completion_ratio,
            image_ratio,
            tiers,
        } => {
            let price = tiers
                .resolve(usage.total_raw())
                .ok_or(PricingError::Internal("empty tier table"))?;
            let set = RatioSet {
                model: mul_ratio(ratio_from_price_per_1m(price)?, tier_r)?,
                completion: *completion_ratio,
                cache: *cache_ratio,
                cache_write: *cache_write_ratio,
                audio: *audio_ratio,
                audio_completion: *audio_completion_ratio,
                image: *image_ratio,
            };
            calc_tokens(book, ctx, usage, &set, group_ratio, "tiered", tier.as_ref())
        }
        PricingMode::PerCall { price } => {
            let effective = if tier_r.as_scaled() == RatioFp::ONE.as_scaled() {
                *price
            } else {
                Money::from_micros(
                    i64::try_from(
                        i128::from(price.as_micros()) * i128::from(tier_r.as_scaled())
                            / i128::from(RATIO_SCALE),
                    )
                    .map_err(|_| PricingError::Overflow)?,
                )
            };
            calc_per_call(book, ctx, effective, group_ratio, tier.as_ref())
        }
    }
}

/// 定点倍率相乘（floor；tier 修饰用）。
fn mul_ratio(a: RatioFp, b: RatioFp) -> Result<RatioFp, PricingError> {
    if b.as_scaled() == RatioFp::ONE.as_scaled() {
        return Ok(a);
    }
    let v = i128::from(a.as_scaled()) * i128::from(b.as_scaled()) / i128::from(RATIO_SCALE);
    RatioFp::from_scaled(i64::try_from(v).map_err(|_| PricingError::Overflow)?)
        .ok_or(PricingError::Overflow)
}

/// 应用用户倍率与规则栈，返回（终值，命中规则）。
fn apply_modifiers(
    book: &PriceBook,
    ctx: &CalcContext,
    value: i128,
) -> Result<(i128, Vec<AppliedRule>), PricingError> {
    let mut value = step(value, ctx.user_multiplier)?;
    let mut applied = Vec::new();
    for rule in book.rules() {
        if rule.applies(ctx) {
            value = step(value, rule.multiplier)?;
            applied.push(AppliedRule {
                code: rule.code.clone(),
                kind: rule.kind.tag(),
                multiplier: rule.multiplier,
            });
        }
    }
    Ok((value, applied))
}

#[allow(clippy::too_many_arguments)]
fn calc_tokens(
    book: &PriceBook,
    ctx: &CalcContext,
    usage: TokenUsage,
    set: &RatioSet,
    group_ratio: RatioFp,
    mode: &'static str,
    tier: Option<&(String, RatioFp)>,
) -> Result<Quote, PricingError> {
    // 有效 token 数（×SCALE 定点），prompt 五段 + completion 两段各乘自己的轴：
    //   uncached×1 + cached×cache + cache_write×cache_write
    // + audio_in×audio + image_in×image
    // + text_out×completion + audio_out×audio×audio_completion
    // 每项 ≤ u32::MAX × 1e6 ≈ 4.3e15（音频输出为两轴叠乘，≤ 4.3e15×1e6 仍远小于
    // i128 上限 1.7e38），checked 仅作防御。
    // 音频输出倍率 = audio × audio_completion（new-api 同语义）。
    //
    // 但两轴均未配置（都是 1.0）时必须**回落到 completion_ratio**：文本输出走
    // completion_ratio（如 4×），若音频输出按 1× 计就成了意外降价——模态轴的缺省值
    // 本应是"零影响"，而非把已有的音频输出打折。
    let audio_out_scaled = if set.audio.as_scaled() == RatioFp::ONE.as_scaled()
        && set.audio_completion.as_scaled() == RatioFp::ONE.as_scaled()
    {
        i128::from(set.completion.as_scaled())
    } else {
        i128::from(set.audio.as_scaled())
            .checked_mul(i128::from(set.audio_completion.as_scaled()))
            .map(|v| v.div_euclid(i128::from(RATIO_SCALE)))
            .ok_or(PricingError::Overflow)?
    };
    let eff = i128::from(usage.prompt_uncached())
        .checked_mul(i128::from(RATIO_SCALE))
        .and_then(|acc| {
            acc.checked_add(i128::from(usage.cached_tokens) * i128::from(set.cache.as_scaled()))
        })
        .and_then(|acc| {
            acc.checked_add(
                i128::from(usage.cache_write_tokens) * i128::from(set.cache_write.as_scaled()),
            )
        })
        .and_then(|acc| {
            acc.checked_add(
                i128::from(usage.audio_prompt_tokens) * i128::from(set.audio.as_scaled()),
            )
        })
        .and_then(|acc| {
            acc.checked_add(
                i128::from(usage.image_prompt_tokens) * i128::from(set.image.as_scaled()),
            )
        })
        .and_then(|acc| {
            acc.checked_add(
                i128::from(usage.text_completion()) * i128::from(set.completion.as_scaled()),
            )
        })
        .and_then(|acc| {
            acc.checked_add(i128::from(usage.audio_completion_tokens) * audio_out_scaled)
        })
        .ok_or(PricingError::Overflow)?;

    let v = step(eff, set.model)?;
    let v = step(v, group_ratio)?;
    let original = micro_from_token_scaled(v)?;

    let (v, applied) = apply_modifiers(book, ctx, v)?;
    let amount = micro_from_token_scaled(v)?;
    let discount = original.checked_sub(amount).ok_or(PricingError::Overflow)?;

    // 最终单价（$/1M input）：基准价走同一条乘链，账单解释器直接展示。
    let mut unit = i128::from(BASE_PRICE_PER_1M_MICRO)
        .checked_mul(i128::from(RATIO_SCALE))
        .ok_or(PricingError::Overflow)?;
    unit = step(unit, set.model)?;
    unit = step(unit, group_ratio)?;
    unit = step(unit, ctx.user_multiplier)?;
    for rule in &applied {
        unit = step(unit, rule.multiplier)?;
    }
    let final_unit = micro_from_money_scaled(unit)?;

    let snapshot = PricingSnapshot {
        epoch: book.epoch(),
        mode,
        model_ratio: Some(set.model),
        completion_ratio: Some(set.completion),
        cache_ratio: Some(set.cache),
        // 各模态轴仅在本次实际发生该段用量时入快照——避免纯文本账单出现一堆
        // 无意义的 1.0 字段，账单解释器也就不必逐个判空
        cache_write_ratio: (usage.cache_write_tokens > 0).then_some(set.cache_write),
        audio_ratio: (usage.audio_prompt_tokens > 0 || usage.audio_completion_tokens > 0)
            .then_some(set.audio),
        audio_completion_ratio: (usage.audio_completion_tokens > 0)
            .then_some(set.audio_completion),
        image_ratio: (usage.image_prompt_tokens > 0).then_some(set.image),
        per_call_price_usd: None,
        service_tier: tier.map(|(t, _)| t.clone()),
        tier_ratio: tier.map(|(_, r)| *r),
        group: ctx.group.to_string(),
        group_ratio,
        user_multiplier: ctx.user_multiplier,
        rules: applied,
        media_units: None,
        final_unit_price_input_per_1m_usd: Some(final_unit),
    };

    Ok(Quote {
        amount,
        original,
        discount,
        snapshot,
    })
}

fn calc_per_call(
    book: &PriceBook,
    ctx: &CalcContext,
    price: Money,
    group_ratio: RatioFp,
    tier: Option<&(String, RatioFp)>,
) -> Result<Quote, PricingError> {
    let v = i128::from(price.as_micros())
        .checked_mul(i128::from(RATIO_SCALE))
        .ok_or(PricingError::Overflow)?;
    let v = step(v, group_ratio)?;
    let original = micro_from_money_scaled(v)?;

    let (v, applied) = apply_modifiers(book, ctx, v)?;
    let amount = micro_from_money_scaled(v)?;
    let discount = original.checked_sub(amount).ok_or(PricingError::Overflow)?;

    let snapshot = PricingSnapshot {
        epoch: book.epoch(),
        mode: "per_call",
        model_ratio: None,
        completion_ratio: None,
        cache_ratio: None,
        cache_write_ratio: None,
        audio_ratio: None,
        audio_completion_ratio: None,
        image_ratio: None,
        per_call_price_usd: Some(price),
        service_tier: tier.map(|(t, _)| t.clone()),
        tier_ratio: tier.map(|(_, r)| *r),
        group: ctx.group.to_string(),
        group_ratio,
        user_multiplier: ctx.user_multiplier,
        rules: applied,
        media_units: None,
        final_unit_price_input_per_1m_usd: None,
    };

    Ok(Quote {
        amount,
        original,
        discount,
        snapshot,
    })
}
