//! 模型定价配置（model_pricing 的领域表示）。

use crate::error::CompileError;
use crate::ratio::{RatioFp, RatioParseError, parse_scaled_1e6};
use okapi_domain::Money;

/// 定价模式（M0 覆盖 ratio / per_call / tiered；media / time 在 M3 扩展）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricingMode {
    /// 倍率制：DESIGN §3.2 统一公式。
    Ratio {
        model_ratio: RatioFp,
        completion_ratio: RatioFp,
        /// 缓存**读取**倍率（命中折扣，Anthropic 官方 0.1×）。
        cache_ratio: RatioFp,
        /// 缓存**写入**倍率（Anthropic 官方 1.25×@5m；缺省 1.0 = 按常规输入计）。
        cache_write_ratio: RatioFp,
    },
    /// 按次计费：`per_call_price × 分组 × 个人 × 规则`。
    PerCall { price: Money },
    /// 阶梯计费：按原始总 tokens 落档取 $/1M 单价，代入 model_ratio 位置。
    Tiered {
        completion_ratio: RatioFp,
        cache_ratio: RatioFp,
        cache_write_ratio: RatioFp,
        tiers: TierTable,
    },
}

impl PricingMode {
    /// 快照用模式标签。
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Ratio { .. } => "ratio",
            Self::PerCall { .. } => "per_call",
            Self::Tiered { .. } => "tiered",
        }
    }

    pub(crate) fn validate(&self, model: &str) -> Result<(), CompileError> {
        match self {
            Self::Ratio { .. } | Self::PerCall { .. } => Ok(()),
            Self::Tiered { tiers, .. } => tiers.validate(model),
        }
    }
}

/// 单个阶梯：`from_tokens` 起（含）适用 `price_per_1m`（$/1M tokens，micro）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    pub from_tokens: u64,
    pub price_per_1m: Money,
}

/// 阶梯表：首档必须从 0 开始，严格升序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierTable(Vec<Tier>);

impl TierTable {
    #[must_use]
    pub fn new(tiers: Vec<Tier>) -> Self {
        Self(tiers)
    }

    /// 解析 Okapi 规范的 tier_expr：`"0:2.5,128000:5"`（from_tokens:USD_per_1M）。
    pub fn parse(expr: &str) -> Result<Self, RatioParseError> {
        let mut tiers = Vec::new();
        for segment in expr.split(',') {
            let (from, price) = segment
                .split_once(':')
                .ok_or(RatioParseError::InvalidChar)?;
            let from_tokens: u64 = from
                .trim()
                .parse()
                .map_err(|_| RatioParseError::InvalidChar)?;
            let price_per_1m = Money::from_micros(parse_scaled_1e6(price)?);
            tiers.push(Tier {
                from_tokens,
                price_per_1m,
            });
        }
        Ok(Self(tiers))
    }

    fn validate(&self, model: &str) -> Result<(), CompileError> {
        let Some(first) = self.0.first() else {
            return Err(CompileError::InvalidTierTable {
                model: model.to_owned(),
                reason: "empty",
            });
        };
        if first.from_tokens != 0 {
            return Err(CompileError::InvalidTierTable {
                model: model.to_owned(),
                reason: "first tier must start at 0",
            });
        }
        if !self
            .0
            .windows(2)
            .all(|w| w[0].from_tokens < w[1].from_tokens)
        {
            return Err(CompileError::InvalidTierTable {
                model: model.to_owned(),
                reason: "tiers must be strictly ascending",
            });
        }
        Ok(())
    }

    /// 按总 tokens 落档（最后一个 `from_tokens <= total` 的档位）。
    /// 前提：已通过 validate（首档从 0 起），故必有档位命中。
    pub(crate) fn resolve(&self, total_tokens: u64) -> Option<Money> {
        self.0
            .iter()
            .rev()
            .find(|tier| tier.from_tokens <= total_tokens)
            .map(|tier| tier.price_per_1m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_resolve_tiers() -> Result<(), RatioParseError> {
        let table = TierTable::parse("0:2.5,128000:5")?;
        assert_eq!(table.resolve(0), Some(Money::from_micros(2_500_000)));
        assert_eq!(table.resolve(127_999), Some(Money::from_micros(2_500_000)));
        assert_eq!(table.resolve(128_000), Some(Money::from_micros(5_000_000)));
        assert_eq!(
            table.resolve(1_000_000),
            Some(Money::from_micros(5_000_000))
        );
        Ok(())
    }

    #[test]
    fn validate_rejects_bad_tables() -> Result<(), RatioParseError> {
        let not_from_zero = TierTable::parse("10:2.5")?;
        assert!(not_from_zero.validate("m").is_err());

        let not_ascending = TierTable::parse("0:2.5,100:3,100:4")?;
        assert!(not_ascending.validate("m").is_err());

        let empty = TierTable::new(Vec::new());
        assert!(empty.validate("m").is_err());
        Ok(())
    }
}
