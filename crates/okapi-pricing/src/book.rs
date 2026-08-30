//! PriceBook：配置时编译产物，请求路径 O(1) 查表（DESIGN §3.3）。

use crate::error::{CompileError, PricingError};
use crate::model::PricingMode;
use crate::ratio::{RATIO_SCALE, RatioFp};
use crate::rules::PricingRule;
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

    Ok(PriceBook {
        epoch: source.epoch,
        models,
        tiers,
        groups,
        overrides,
        rules,
    })
}

/// absolute 专属价 → 倍率三元组（DESIGN §3.2 换算，整数定点，floor）。
fn absolute_to_ratio(
    user: UserId,
    model: &ModelCode,
    input_per_1m: Money,
    output_per_1m: Money,
    cache_ratio: RatioFp,
    cache_write_ratio: RatioFp,
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

    Ok(PricingMode::Ratio {
        model_ratio,
        completion_ratio,
        cache_ratio,
        cache_write_ratio,
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

    #[test]
    fn compile_rejects_duplicates() {
        let entry = ModelEntry {
            model: ModelCode::from("m"),
            pricing: PricingMode::Ratio {
                model_ratio: RatioFp::ONE,
                completion_ratio: RatioFp::ONE,
                cache_ratio: RatioFp::ONE,
                cache_write_ratio: RatioFp::ONE,
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
