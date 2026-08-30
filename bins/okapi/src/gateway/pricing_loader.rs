//! PG 定价行 → PriceBookSource 翻译（装配层；okapi-pricing 不依赖存储）。
//! 非法配置条目：跳过 + 告警（单条脏配置不应导致整表不可用），
//! 但编译失败（重复键等结构性错误）会向上抛出 —— fail-closed。

use okapi_domain::{GroupCode, ModelCode, Money, UserId};
use okapi_pricing::{
    GroupEntry, ModelEntry, OverrideEntry, OverrideSpec, PriceBook, PriceBookSource, PricingMode,
    PricingRule, RatioFp, RuleKind, RuleScope, TierTable, book,
};
use okapi_store::pricing::{ModelPricingRow, PricingSourceRows, RuleRow, UserPricingRow};
use sqlx::PgPool;

fn fp(scaled: i64) -> Option<RatioFp> {
    RatioFp::from_scaled(scaled)
}

fn row_to_mode(row: &ModelPricingRow) -> Option<PricingMode> {
    match row.pricing_mode.as_str() {
        "ratio" => Some(PricingMode::Ratio {
            model_ratio: fp(row.model_ratio_scaled?)?,
            completion_ratio: fp(row.completion_ratio_scaled)?,
            cache_ratio: fp(row.cache_ratio_scaled)?,
            cache_write_ratio: fp(row.cache_write_ratio_scaled)?,
        }),
        "per_call" => Some(PricingMode::PerCall {
            price: Money::from_micros(row.per_call_price_micro?),
        }),
        "tiered" => Some(PricingMode::Tiered {
            completion_ratio: fp(row.completion_ratio_scaled)?,
            cache_ratio: fp(row.cache_ratio_scaled)?,
            cache_write_ratio: fp(row.cache_write_ratio_scaled)?,
            tiers: TierTable::parse(row.tier_expr.as_deref()?).ok()?,
        }),
        _ => None,
    }
}

fn row_to_override(row: &UserPricingRow) -> Option<OverrideSpec> {
    match row.override_kind.as_str() {
        "ratio" => Some(OverrideSpec::Ratio(PricingMode::Ratio {
            model_ratio: fp(row.custom_model_ratio_scaled?)?,
            completion_ratio: fp(row.custom_completion_ratio_scaled.unwrap_or(1_000_000))?,
            cache_ratio: fp(row.custom_cache_ratio_scaled.unwrap_or(1_000_000))?,
            cache_write_ratio: fp(row.custom_cache_write_ratio_scaled.unwrap_or(1_000_000))?,
        })),
        "absolute" => Some(OverrideSpec::Absolute {
            input_per_1m: Money::from_micros(row.custom_input_per_1m_micro?),
            output_per_1m: Money::from_micros(row.custom_output_per_1m_micro?),
            cache_ratio: fp(row.custom_cache_ratio_scaled.unwrap_or(1_000_000))?,
            cache_write_ratio: fp(row.custom_cache_write_ratio_scaled.unwrap_or(1_000_000))?,
        }),
        _ => None,
    }
}

fn ratio_from_json(value: Option<&serde_json::Value>) -> Option<RatioFp> {
    let value = value?;
    let literal = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    literal.parse().ok()
}

fn u64_from_json(value: Option<&serde_json::Value>) -> Option<u64> {
    value?.as_u64()
}

fn u16_from_json(value: Option<&serde_json::Value>) -> Option<u16> {
    value?.as_u64().and_then(|v| u16::try_from(v).ok())
}

fn scope_from_json(value: &serde_json::Value) -> RuleScope {
    let list = |key: &str| -> Option<Vec<String>> {
        value.get(key)?.as_array().map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
    };
    RuleScope {
        groups: list("groups").map(|v| v.into_iter().map(GroupCode::from).collect()),
        models: list("models").map(|v| v.into_iter().map(ModelCode::from).collect()),
        users: value.get("users").and_then(|u| u.as_array()).map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .map(UserId::new)
                .collect()
        }),
    }
}

fn row_to_rule(row: &RuleRow) -> Option<PricingRule> {
    let multiplier = ratio_from_json(row.params.get("multiplier"))?;
    let kind = match row.rule_type.as_str() {
        "volume" => RuleKind::Volume {
            min_monthly_tokens: u64_from_json(row.params.get("min_monthly_tokens"))?,
        },
        "time_based" => RuleKind::TimeBased {
            start_minute: u16_from_json(row.params.get("start_minute"))?,
            end_minute: u16_from_json(row.params.get("end_minute"))?,
        },
        "discount" => RuleKind::Discount,
        "surge" => RuleKind::Surge,
        _ => return None,
    };
    Some(PricingRule {
        code: row.rule_code.clone(),
        kind,
        multiplier,
        scope: scope_from_json(&row.scope),
        priority: row.priority,
        valid_from: row.valid_from.map(|t| t.timestamp()),
        valid_to: row.valid_to.map(|t| t.timestamp()),
    })
}

/// 行集 → 编译源（脏条目跳过并告警）。
#[must_use]
pub fn build_source(rows: &PricingSourceRows) -> PriceBookSource {
    let mut models = Vec::with_capacity(rows.models.len());
    for row in &rows.models {
        if let Some(pricing) = row_to_mode(row) {
            let tier_ratios = row
                .tier_ratios
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| {
                            let s = v
                                .as_str()
                                .map(str::to_owned)
                                .or_else(|| v.as_f64().map(|f| f.to_string()))?;
                            s.parse::<RatioFp>().ok().map(|r| (k.clone(), r))
                        })
                        .collect()
                })
                .unwrap_or_default();
            models.push(ModelEntry {
                model: ModelCode::from(row.model_name.as_str()),
                pricing,
                tier_ratios,
            });
        } else {
            tracing::warn!(model = %row.model_name, "跳过非法模型定价行");
        }
    }

    let groups = rows
        .groups
        .iter()
        .filter_map(|row| {
            let ratio = fp(row.ratio_scaled)?;
            Some(GroupEntry {
                group: GroupCode::from(row.group_code.as_str()),
                ratio,
            })
        })
        .collect();

    let mut overrides = Vec::new();
    for row in &rows.overrides {
        if let Some(spec) = row_to_override(row) {
            overrides.push(OverrideEntry {
                user: UserId::new(row.user_id),
                model: ModelCode::from(row.model_name.as_str()),
                spec,
            });
        } else {
            tracing::warn!(user = row.user_id, model = %row.model_name, "跳过非法用户定价行");
        }
    }

    let mut rules = Vec::new();
    for row in &rows.rules {
        if let Some(rule) = row_to_rule(row) {
            rules.push(rule);
        } else {
            tracing::warn!(rule = %row.rule_code, "跳过非法定价规则");
        }
    }

    PriceBookSource {
        epoch: rows.epoch,
        models,
        groups,
        overrides,
        rules,
    }
}

/// 全量装载并编译 PriceBook（启动与热更共用）。
pub async fn load_pricebook(pool: &PgPool) -> anyhow::Result<PriceBook> {
    let rows = okapi_store::pricing::load_pricing_source_rows(pool).await?;
    let source = build_source(&rows);
    let compiled = book::compile(source).map_err(|e| anyhow::anyhow!("pricebook compile: {e}"))?;
    Ok(compiled)
}
