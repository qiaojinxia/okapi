//! pricing_snapshot：每笔账可解释（DESIGN §3.4），落 billing_records.pricing_snapshot。
//!
//! 倍率与单价序列化为**精确 JSON 数字**（serde_json arbitrary_precision），
//! 全程不经过浮点。

use crate::ratio::RatioFp;
use okapi_domain::Money;
use serde::ser::Error as _;
use serde::{Serialize, Serializer};
use std::str::FromStr;

fn ser_decimal<S: Serializer>(literal: &str, serializer: S) -> Result<S::Ok, S::Error> {
    let number = serde_json::Number::from_str(literal).map_err(S::Error::custom)?;
    number.serialize(serializer)
}

// serde 的 serialize_with 契约固定按引用传字段。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn ser_ratio<S: Serializer>(ratio: &RatioFp, serializer: S) -> Result<S::Ok, S::Error> {
    ser_decimal(&ratio.to_string(), serializer)
}

// serde 的 serialize_with 契约固定传 `&Option<T>`，此处不适用 ref_option 建议。
#[allow(clippy::ref_option)]
fn ser_ratio_opt<S: Serializer>(ratio: &Option<RatioFp>, serializer: S) -> Result<S::Ok, S::Error> {
    match ratio {
        Some(inner) => ser_ratio(inner, serializer),
        None => serializer.serialize_none(),
    }
}

#[allow(clippy::ref_option)]
fn ser_usd_opt<S: Serializer>(money: &Option<Money>, serializer: S) -> Result<S::Ok, S::Error> {
    match money {
        Some(inner) => ser_decimal(&inner.to_usd_string(), serializer),
        None => serializer.serialize_none(),
    }
}

/// 命中并施加的规则（顺序即施加顺序）。
#[derive(Debug, Clone, Serialize)]
pub struct AppliedRule {
    pub code: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(serialize_with = "ser_ratio")]
    pub multiplier: RatioFp,
}

/// 计费快照：审计可回放、账单解释器数据源。
#[derive(Debug, Clone, Serialize)]
pub struct PricingSnapshot {
    pub epoch: i64,
    /// ratio / per_call / tiered。
    pub mode: &'static str,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_ratio_opt"
    )]
    pub model_ratio: Option<RatioFp>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_ratio_opt"
    )]
    pub completion_ratio: Option<RatioFp>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_ratio_opt"
    )]
    pub cache_ratio: Option<RatioFp>,
    /// 缓存写入倍率（仅本次实际发生缓存写入时出现，Anthropic 系）。
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_ratio_opt"
    )]
    pub cache_write_ratio: Option<RatioFp>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_usd_opt"
    )]
    pub per_call_price_usd: Option<Money>,
    /// service_tier 结算档位（None = 未启用/default，DESIGN §3-4.5）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_ratio_opt"
    )]
    pub tier_ratio: Option<RatioFp>,
    pub group: String,
    #[serde(serialize_with = "ser_ratio")]
    pub group_ratio: RatioFp,
    #[serde(serialize_with = "ser_ratio")]
    pub user_multiplier: RatioFp,
    pub rules: Vec<AppliedRule>,
    /// 媒体单位数（图像张数等；per_call × n 的乘数，M3 媒体计费）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_units: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_usd_opt"
    )]
    pub final_unit_price_input_per_1m_usd: Option<Money>,
}
