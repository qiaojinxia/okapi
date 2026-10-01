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

pub const BASE_PRICE_SETTING: &str = "pricing_base_per_1m_micro";

pub fn valid_base_price(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .filter(|v| (1..=book::MAX_BASE_PRICE_PER_1M_MICRO).contains(v))
}

fn parse_base_price(value: Option<serde_json::Value>) -> Result<i64, okapi_store::StoreError> {
    match value {
        None => Ok(book::BASE_PRICE_PER_1M_MICRO),
        Some(value) => valid_base_price(&value).ok_or(okapi_store::StoreError::InvalidData(
            "pricing_base_per_1m_micro",
        )),
    }
}

/// Settings are a draft; gateway startup/reloads must never activate an unpublished base.
pub async fn draft_base_price(pool: &PgPool) -> Result<i64, okapi_store::StoreError> {
    parse_base_price(
        sqlx::query_scalar::<_, serde_json::Value>("SELECT value FROM settings WHERE key = $1")
            .bind(BASE_PRICE_SETTING)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn published_base_price(
    pool: &PgPool,
    epoch: Option<i64>,
) -> Result<i64, okapi_store::StoreError> {
    let value: Option<Option<serde_json::Value>> = sqlx::query_scalar(
        "SELECT snapshot -> 'base_price_per_1m_micro' FROM pricing_epochs WHERE ($1::bigint IS NULL OR epoch = $1) ORDER BY epoch DESC LIMIT 1"
    ).bind(epoch).fetch_optional(pool).await?;
    parse_base_price(value.flatten())
}

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
            audio_ratio: fp(row.audio_ratio_scaled)?,
            audio_completion_ratio: fp(row.audio_completion_ratio_scaled)?,
            image_ratio: fp(row.image_ratio_scaled)?,
            modality_ratios: okapi_pricing::ModalityRatios::parse(&row.modality_ratios).ok()?,
        }),
        "per_call" => Some(PricingMode::PerCall {
            price: Money::from_micros(row.per_call_price_micro?),
        }),
        "tiered" => Some(PricingMode::Tiered {
            completion_ratio: fp(row.completion_ratio_scaled)?,
            cache_ratio: fp(row.cache_ratio_scaled)?,
            cache_write_ratio: fp(row.cache_write_ratio_scaled)?,
            audio_ratio: fp(row.audio_ratio_scaled)?,
            audio_completion_ratio: fp(row.audio_completion_ratio_scaled)?,
            image_ratio: fp(row.image_ratio_scaled)?,
            modality_ratios: okapi_pricing::ModalityRatios::parse(&row.modality_ratios).ok()?,
            tiers: TierTable::parse(row.tier_expr.as_deref()?).ok()?,
        }),
        _ => None,
    }
}

fn row_to_override(row: &UserPricingRow, model: Option<&ModelPricingRow>) -> Option<OverrideSpec> {
    let cache_write = row
        .custom_cache_write_ratio_scaled
        .unwrap_or_else(|| model.map_or(1_000_000, |m| m.cache_write_ratio_scaled));
    match row.override_kind.as_str() {
        "ratio" => Some(OverrideSpec::Ratio(PricingMode::Ratio {
            model_ratio: fp(row.custom_model_ratio_scaled?)?,
            completion_ratio: fp(row.custom_completion_ratio_scaled.unwrap_or(1_000_000))?,
            cache_ratio: fp(row.custom_cache_ratio_scaled.unwrap_or(1_000_000))?,
            cache_write_ratio: fp(cache_write)?,
            // 模态轴不做用户级覆盖：它表达"音频相对文本的倍数"，属模型固有属性
            audio_ratio: fp(model.map_or(1_000_000, |m| m.audio_ratio_scaled))?,
            audio_completion_ratio: fp(
                model.map_or(1_000_000, |m| m.audio_completion_ratio_scaled)
            )?,
            image_ratio: fp(model.map_or(1_000_000, |m| m.image_ratio_scaled))?,
            modality_ratios: match model {
                Some(model) => okapi_pricing::ModalityRatios::parse(&model.modality_ratios).ok()?,
                None => okapi_pricing::ModalityRatios::default(),
            },
        })),
        "absolute" => Some(OverrideSpec::Absolute {
            input_per_1m: Money::from_micros(row.custom_input_per_1m_micro?),
            output_per_1m: Money::from_micros(row.custom_output_per_1m_micro?),
            cache_ratio: fp(row.custom_cache_ratio_scaled.unwrap_or(1_000_000))?,
            cache_write_ratio: fp(cache_write)?,
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
        "volume" => {
            // 双阈值轴（AND；0 = 该轴不设）。两轴全空 = 无条件规则冒充 volume，
            // 属配置错误 → 按脏行跳过（console 写入口会拦，这里防直写库）
            let min_monthly_tokens =
                u64_from_json(row.params.get("min_monthly_tokens")).unwrap_or(0);
            let min_monthly_spend_micro =
                u64_from_json(row.params.get("min_monthly_spend_micro")).unwrap_or(0);
            if min_monthly_tokens == 0 && min_monthly_spend_micro == 0 {
                return None;
            }
            RuleKind::Volume {
                min_monthly_tokens,
                min_monthly_spend_micro,
            }
        }
        "time_based" => RuleKind::TimeBased {
            start_minute: u16_from_json(row.params.get("start_minute"))?,
            end_minute: u16_from_json(row.params.get("end_minute"))?,
            weekdays: weekdays_from_json(row.params.get("weekdays"))?,
        },
        "discount" => RuleKind::Discount,
        "surge" => RuleKind::Surge,
        _ => return None,
    };
    // 未知 stacking_mode 按脏行跳过（fail-closed）：静默当 stackable 会让本应
    // 排他的活动错误叠加，造成超额折扣——老 ok-api 同一决策
    let stacking = match row.params.get("stacking_mode").and_then(|v| v.as_str()) {
        None => okapi_pricing::Stacking::Stackable,
        Some(raw) => okapi_pricing::Stacking::parse(raw)?,
    };
    Some(PricingRule {
        code: row.rule_code.clone(),
        kind,
        multiplier,
        scope: scope_from_json(&row.scope),
        priority: row.priority,
        stacking,
        valid_from: row.valid_from.map(|t| t.timestamp()),
        valid_to: row.valid_to.map(|t| t.timestamp()),
    })
}

/// params.weekdays（0–6 数组）→ 掩码；缺省 = 每天；非法值/空数组 = 脏行（None）。
fn weekdays_from_json(value: Option<&serde_json::Value>) -> Option<okapi_pricing::WeekdayMask> {
    let Some(value) = value else {
        return Some(okapi_pricing::WeekdayMask::ALL);
    };
    let days: Vec<u8> = value
        .as_array()?
        .iter()
        .map(|v| v.as_u64().and_then(|d| u8::try_from(d).ok()))
        .collect::<Option<Vec<u8>>>()?;
    okapi_pricing::WeekdayMask::from_days(&days)
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

    let model_rows: std::collections::HashMap<_, _> = rows
        .models
        .iter()
        .map(|row| (row.model_name.as_str(), row))
        .collect();
    let mut overrides = Vec::new();
    for row in &rows.overrides {
        let model = model_rows.get(row.model_name.as_str()).copied();
        if let Some(spec) = row_to_override(row, model) {
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

pub fn publication_base_price(
    publication: &okapi_store::pricing::PublishedPricing,
) -> Result<i64, okapi_store::StoreError> {
    parse_base_price(publication.base_price_per_1m_micro.clone())
}

/// Only a published snapshot can activate prices, even after restart/cache clear.
pub async fn load_pricebook(pool: &PgPool) -> anyhow::Result<PriceBook> {
    let mut conn = pool.acquire().await?;
    let publication = okapi_store::pricing::published_pricing(&mut conn).await?;
    let source = build_source(&publication.source);
    let base = publication_base_price(&publication)?;
    let compiled = book::compile_with_base(source, base)
        .map_err(|e| anyhow::anyhow!("pricebook compile: {e}"))?;
    Ok(compiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_only_prices_remain_available_without_a_base_model_price() {
        for kind in ["ratio", "absolute"] {
            let rows = PricingSourceRows {
                epoch: 1,
                models: vec![],
                rules: vec![],
                groups: vec![okapi_store::pricing::GroupRow {
                    group_code: "default".into(),
                    ratio_scaled: 1_000_000,
                }],
                overrides: vec![UserPricingRow {
                    user_id: 1,
                    model_name: "m".into(),
                    override_kind: kind.into(),
                    custom_model_ratio_scaled: Some(5_000_000),
                    custom_completion_ratio_scaled: Some(4_000_000),
                    custom_cache_ratio_scaled: Some(250_000),
                    custom_cache_write_ratio_scaled: None,
                    custom_input_per_1m_micro: Some(10_000_000),
                    custom_output_per_1m_micro: Some(40_000_000),
                }],
            };
            let book = book::compile(build_source(&rows)).unwrap();
            let quote = okapi_pricing::calculate(
                &book,
                &okapi_pricing::CalcContext {
                    user: UserId::new(1),
                    model: "m".into(),
                    group: "default".into(),
                    user_multiplier: RatioFp::ONE,
                    monthly_tokens: 0,
                    monthly_spend_micro: 0,
                    local_minute_of_day: 0,
                    now_unix: 0,
                    surge_active: false,
                    service_tier: None,
                },
                okapi_domain::TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    ..okapi_domain::TokenUsage::default()
                },
            )
            .unwrap();
            assert_eq!(quote.amount.as_micros(), 50);
        }
    }
}
