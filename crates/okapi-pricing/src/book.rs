//! PriceBook：配置时编译产物，请求路径 O(1) 查表（DESIGN §3.3）。

use crate::error::{CompileError, PricingError};
use crate::model::PricingMode;
use crate::ratio::{RATIO_SCALE, RatioFp};
use crate::rules::{PricingRule, RuleKind};
use okapi_domain::{GroupCode, ModelCode, Money, UserId};
use std::collections::HashMap;

/// 基准价 $2/1M input tokens 的 micro 表示（与 new-api 倍率基准对齐）。
pub(crate) const BASE_PRICE_PER_1M_MICRO: i64 = 2_000_000;

/// 编译输入：模型定价条目（model_pricing 行）。
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub model: ModelCode,
    pub pricing: PricingMode,
    /// service_tier 档位倍率（空 = 全档 1.0，DESIGN §3-4.5）。
    pub tier_ratios: Vec<(String, RatioFp)>,
}

/// 编译输入：分组倍率条目（price_groups 行）。
#[derive(Debug, Clone)]
pub struct GroupEntry {
    pub group: GroupCode,
    pub ratio: RatioFp,
}

/// 用户专属覆盖（user_pricing 行）：ratio 直给，或 absolute 绝对价（编译期换算为倍率）。
#[derive(Debug, Clone)]
pub enum OverrideSpec {
    Ratio(PricingMode),
    Absolute {
        input_per_1m: Money,
        output_per_1m: Money,
        cache_ratio: RatioFp,
        cache_write_ratio: RatioFp,
    },
}

/// 编译输入：用户专属覆盖条目。
#[derive(Debug, Clone)]
pub struct OverrideEntry {
    pub user: UserId,
    pub model: ModelCode,
    pub spec: OverrideSpec,
}

/// 编译输入全集（M2 起由 console 从 PG 读出并发布）。
#[derive(Debug, Clone)]
pub struct PriceBookSource {
    pub epoch: i64,
    pub models: Vec<ModelEntry>,
    pub groups: Vec<GroupEntry>,
    pub overrides: Vec<OverrideEntry>,
    pub rules: Vec<PricingRule>,
}

/// 已编译价格表（进程内经 ArcSwap 只读共享）。
#[derive(Debug, Clone)]
pub struct PriceBook {
    epoch: i64,
    models: HashMap<ModelCode, PricingMode>,
    /// service_tier 档位倍率（仅存配置了的模型）。
    tiers: HashMap<ModelCode, HashMap<String, RatioFp>>,
    groups: HashMap<GroupCode, RatioFp>,
    overrides: HashMap<(UserId, ModelCode), PricingMode>,
    /// 已按 类间固定序 → priority → code 排序。
    rules: Vec<PricingRule>,
    /// 编译期算好的触发输入门控：无此类规则时 gateway 不采集对应输入（热路径零开销）。
    has_volume_rules: bool,
    /// volume 规则的消费额轴（min_monthly_spend_micro > 0）单独门控：
    /// 只用 token 阈值的站点不为消费额计数付 Redis 往返。
    has_spend_rules: bool,
    has_surge_rules: bool,
}

/// 解析结果：用户专属覆盖优先于模型定价（DESIGN §3.4 优先级 1–2）。
pub(crate) struct ResolvedRate<'a> {
    pub pricing: &'a PricingMode,
    pub group_ratio: RatioFp,
}

/// 编译：校验 + 规则排序 + absolute 换算。任何配置错误 fail-closed 拒绝发布。
pub fn compile(source: PriceBookSource) -> Result<PriceBook, CompileError> {
    let mut models = HashMap::with_capacity(source.models.len());
    let mut tiers: HashMap<ModelCode, HashMap<String, RatioFp>> = HashMap::new();
    for entry in source.models {
        entry.pricing.validate(entry.model.as_str())?;
        let key = entry.model.clone();
        if !entry.tier_ratios.is_empty() {
            tiers.insert(key.clone(), entry.tier_ratios.iter().cloned().collect());
        }
        if models.insert(key, entry.pricing).is_some() {
            return Err(CompileError::DuplicateModel(entry.model.to_string()));
        }
    }

    let mut groups = HashMap::with_capacity(source.groups.len());
    for entry in source.groups {
        let key = entry.group.clone();
        if groups.insert(key, entry.ratio).is_some() {
            return Err(CompileError::DuplicateGroup(entry.group.to_string()));
        }
    }

    let mut overrides = HashMap::with_capacity(source.overrides.len());
    for entry in source.overrides {
        let pricing = match entry.spec {
            OverrideSpec::Ratio(mode) => {
                mode.validate(entry.model.as_str())?;
                mode
            }
            OverrideSpec::Absolute {
                input_per_1m,
                output_per_1m,
                cache_ratio,
                cache_write_ratio,
            } => absolute_to_ratio(
                entry.user,
                &entry.model,
                input_per_1m,
                output_per_1m,
                cache_ratio,
                cache_write_ratio,
                // 模态轴从模型级继承：audio_ratio 是"音频相对文本的倍数"，属模型固有
                // 属性（gpt-4o-audio 音频恒为文本 16×），不因用户而变。专属绝对价只
                // 覆盖文本单价；若退化为 1.0，专属价大客户用音频会严重少收。
                models.get(&entry.model).map(modal_axes),
            )?,
        };
        if overrides
            .insert((entry.user, entry.model.clone()), pricing)
            .is_some()
        {
            return Err(CompileError::DuplicateOverride {
                user: entry.user.get(),
                model: entry.model.to_string(),
            });
        }
    }

    let mut rules = source.rules;
    rules.sort_by(|a, b| {
        (a.kind.order(), a.priority, a.code.as_str()).cmp(&(
            b.kind.order(),
            b.priority,
            b.code.as_str(),
        ))
    });

    let has_volume_rules = rules
        .iter()
        .any(|r| matches!(r.kind, RuleKind::Volume { .. }));
    let has_spend_rules = rules.iter().any(|r| {
        matches!(
            r.kind,
            RuleKind::Volume {
                min_monthly_spend_micro: 1..,
                ..
            }
        )
    });
    let has_surge_rules = rules.iter().any(|r| matches!(r.kind, RuleKind::Surge));

    Ok(PriceBook {
        epoch: source.epoch,
        models,
        tiers,
        groups,
        overrides,
        rules,
        has_volume_rules,
        has_spend_rules,
        has_surge_rules,
    })
}

/// absolute 专属价 → 倍率三元组（DESIGN §3.2 换算，整数定点，floor）。
/// 从已编译的模型定价里取出三条模态轴（per_call 模式无 token 轴 → 全 1.0）。
fn modal_axes(mode: &PricingMode) -> (RatioFp, RatioFp, RatioFp) {
    match mode {
        PricingMode::Ratio {
            audio_ratio,
            audio_completion_ratio,
            image_ratio,
            ..
        }
        | PricingMode::Tiered {
            audio_ratio,
            audio_completion_ratio,
            image_ratio,
            ..
        } => (*audio_ratio, *audio_completion_ratio, *image_ratio),
        PricingMode::PerCall { .. } => (RatioFp::ONE, RatioFp::ONE, RatioFp::ONE),
    }
}

#[allow(clippy::too_many_arguments)]
fn absolute_to_ratio(
    user: UserId,
    model: &ModelCode,
    input_per_1m: Money,
    output_per_1m: Money,
    cache_ratio: RatioFp,
    cache_write_ratio: RatioFp,
    // 模型级模态轴；None = 该模型无定价行（罕见，按 1.0 处理）
    axes: Option<(RatioFp, RatioFp, RatioFp)>,
) -> Result<PricingMode, CompileError> {
    let input = input_per_1m.as_micros();
    let output = output_per_1m.as_micros();
    let invalid = |reason: &'static str| CompileError::InvalidAbsoluteOverride {
        user: user.get(),
        model: model.to_string(),
        reason,
    };
    if input <= 0 {
        return Err(invalid("input price must be positive"));
    }
    if output < 0 {
        return Err(invalid("output price must be non-negative"));
    }

    let model_scaled = i128::from(input)
        .checked_mul(i128::from(RATIO_SCALE))
        .map(|v| v.div_euclid(i128::from(BASE_PRICE_PER_1M_MICRO)))
        .ok_or_else(|| invalid("model ratio overflow"))?;
    let completion_scaled = i128::from(output)
        .checked_mul(i128::from(RATIO_SCALE))
        .map(|v| v.div_euclid(i128::from(input)))
        .ok_or_else(|| invalid("completion ratio overflow"))?;

    let model_ratio = i64::try_from(model_scaled)
        .ok()
        .and_then(RatioFp::from_scaled)
        .ok_or_else(|| invalid("model ratio out of range"))?;
    let completion_ratio = i64::try_from(completion_scaled)
        .ok()
        .and_then(RatioFp::from_scaled)
        .ok_or_else(|| invalid("completion ratio out of range"))?;

    let (audio_ratio, audio_completion_ratio, image_ratio) =
        axes.unwrap_or((RatioFp::ONE, RatioFp::ONE, RatioFp::ONE));
    Ok(PricingMode::Ratio {
        model_ratio,
        completion_ratio,
        cache_ratio,
        cache_write_ratio,
        audio_ratio,
        audio_completion_ratio,
        image_ratio,
    })
}

impl PriceBook {
    #[must_use]
    pub const fn epoch(&self) -> i64 {
        self.epoch
    }

    /// 模型是否配置了 service_tier 档位倍率（gateway 据此决定是否采集响应档位）。
    #[must_use]
    pub fn has_tiers(&self, model: &ModelCode) -> bool {
        self.tiers.contains_key(model)
    }

    /// 价簿里是否配了这个模型（不含用户级覆盖）。
    ///
    /// 修饰符变体定价用（§11.25）：`gpt-5@effort:high` 配了价就按变体收，没配就回退基座。
    /// 拿它先问一句，好过用 `resolve` 的 `UnknownModel` 错误当分支条件。
    #[must_use]
    pub fn has_model(&self, model: &ModelCode) -> bool {
        self.models.contains_key(model)
    }

    /// 是否存在启用的 volume 规则（gateway 据此决定是否读 `tok:{uid}:<yyyymm>`）。
    #[must_use]
    pub const fn has_volume_rules(&self) -> bool {
        self.has_volume_rules
    }

    /// 是否存在带消费额阈值的 volume 规则（gateway 据此决定是否读写
    /// `usd:{uid}:<yyyymm>`）。
    #[must_use]
    pub const fn has_spend_rules(&self) -> bool {
        self.has_spend_rules
    }

    /// 是否存在启用的 surge 规则（gateway 据此决定是否读在途计数与阈值设置）。
    #[must_use]
    pub const fn has_surge_rules(&self) -> bool {
        self.has_surge_rules
    }

    /// 档位倍率查询（未配置模型/档位名 → None，调用方按 1.0 处理）。
    #[must_use]
    pub fn tier_ratio(&self, model: &ModelCode, tier: &str) -> Option<RatioFp> {
        self.tiers.get(model).and_then(|m| m.get(tier).copied())
    }

    pub(crate) fn resolve(
        &self,
        user: UserId,
        model: &ModelCode,
        group: &GroupCode,
    ) -> Result<ResolvedRate<'_>, PricingError> {
        let pricing = self
            .overrides
            .get(&(user, model.clone()))
            .or_else(|| self.models.get(model))
            .ok_or_else(|| PricingError::UnknownModel(model.to_string()))?;
        let group_ratio = *self
            .groups
            .get(group)
            .ok_or_else(|| PricingError::UnknownGroup(group.to_string()))?;
        Ok(ResolvedRate {
            pricing,
            group_ratio,
        })
    }

    pub(crate) fn rules(&self) -> &[PricingRule] {
        &self.rules
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{RuleKind, RuleScope};

    fn rule(code: &str, kind: RuleKind, priority: i32) -> PricingRule {
        PricingRule {
            code: code.to_owned(),
            kind,
            multiplier: RatioFp::ONE,
            scope: RuleScope::default(),
            priority,
            stacking: crate::rules::Stacking::Stackable,
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn absolute_override_converts_to_ratios() -> Result<(), CompileError> {
        // GPT-4o：$2.5 / $10 per 1M → model_ratio 1.25，completion_ratio 4
        let mode = absolute_to_ratio(
            UserId::new(1),
            &ModelCode::from("gpt-4o"),
            Money::from_micros(2_500_000),
            Money::from_micros(10_000_000),
            RatioFp::ONE,
            RatioFp::ONE,
            None,
        )?;
        let PricingMode::Ratio {
            model_ratio,
            completion_ratio,
            ..
        } = mode
        else {
            return Err(CompileError::InvalidAbsoluteOverride {
                user: 1,
                model: "gpt-4o".to_owned(),
                reason: "expected ratio mode",
            });
        };
        assert_eq!(model_ratio.as_scaled(), 1_250_000);
        assert_eq!(completion_ratio.as_scaled(), 4_000_000);
        Ok(())
    }

    #[test]
    fn compile_sorts_rules_by_kind_then_priority_then_code() -> Result<(), CompileError> {
        let source = PriceBookSource {
            epoch: 1,
            models: Vec::new(),
            groups: Vec::new(),
            overrides: Vec::new(),
            rules: vec![
                rule("z-surge", RuleKind::Surge, 0),
                rule("b-discount", RuleKind::Discount, 5),
                rule("a-discount", RuleKind::Discount, 5),
                rule(
                    "vol",
                    RuleKind::Volume {
                        min_monthly_tokens: 1,
                        min_monthly_spend_micro: 0,
                    },
                    9,
                ),
            ],
        };
        let book = compile(source)?;
        let codes: Vec<&str> = book.rules().iter().map(|r| r.code.as_str()).collect();
        assert_eq!(codes, ["vol", "a-discount", "b-discount", "z-surge"]);
        Ok(())
    }

    /// 触发输入门控：gateway 据此决定是否采集 volume/surge 输入，
    /// 误判为 false 会让规则静默失效，误判为 true 则给未用该能力的站点加热路径开销。
    #[test]
    fn compile_flags_rule_kinds_needing_runtime_inputs() -> Result<(), CompileError> {
        let empty = compile(PriceBookSource {
            epoch: 1,
            models: Vec::new(),
            groups: Vec::new(),
            overrides: Vec::new(),
            rules: vec![rule("d", RuleKind::Discount, 0)],
        })?;
        assert!(!empty.has_volume_rules(), "仅 discount 规则不需要月度计数");
        assert!(!empty.has_surge_rules());

        let both = compile(PriceBookSource {
            epoch: 1,
            models: Vec::new(),
            groups: Vec::new(),
            overrides: Vec::new(),
            rules: vec![
                rule(
                    "v",
                    RuleKind::Volume {
                        min_monthly_tokens: 1,
                        min_monthly_spend_micro: 0,
                    },
                    0,
                ),
                rule("s", RuleKind::Surge, 0),
            ],
        })?;
        assert!(both.has_volume_rules());
        assert!(both.has_surge_rules());
        assert!(
            !both.has_spend_rules(),
            "仅 token 阈值不该触发消费额计数采集"
        );

        let spend = compile(PriceBookSource {
            epoch: 1,
            models: Vec::new(),
            groups: Vec::new(),
            overrides: Vec::new(),
            rules: vec![rule(
                "sp",
                RuleKind::Volume {
                    min_monthly_tokens: 0,
                    min_monthly_spend_micro: 50_000_000,
                },
                0,
            )],
        })?;
        assert!(spend.has_volume_rules());
        assert!(spend.has_spend_rules());
        Ok(())
    }

    #[test]
    fn compile_rejects_duplicates() {
        let entry = ModelEntry {
            model: ModelCode::from("m"),
            pricing: PricingMode::Ratio {
                model_ratio: RatioFp::ONE,
                completion_ratio: RatioFp::ONE,
                cache_ratio: RatioFp::ONE,
                cache_write_ratio: RatioFp::ONE,
                audio_ratio: RatioFp::ONE,
                audio_completion_ratio: RatioFp::ONE,
                image_ratio: RatioFp::ONE,
            },
            tier_ratios: Vec::new(),
        };
        let source = PriceBookSource {
            epoch: 1,
            models: vec![entry.clone(), entry],
            groups: Vec::new(),
            overrides: Vec::new(),
            rules: Vec::new(),
        };
        assert!(matches!(
            compile(source),
            Err(CompileError::DuplicateModel(_))
        ));
    }
}
