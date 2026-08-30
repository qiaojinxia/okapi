//! 金额类型：micro-USD 整数（$1 = 1_000_000 micro）。
//!
//! 计费路径唯一合法货币载体（DESIGN §3 / IMPLEMENTATION §1.1）。
//! quota 视图（new-api 生态兼容）：1 quota = $2e-6 = 2 micro，仅展示层换算。

use serde::{Deserialize, Serialize};

/// 每美元的 micro 数。
pub const MICROS_PER_USD: i64 = 1_000_000;

/// quota 视图换算：1 quota = 2 micro（$1 = 500_000 quota，与 new-api QuotaPerUnit 对齐）。
pub const MICROS_PER_QUOTA: i64 = 2;

/// 金额（micro-USD，i64）。
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.0
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    #[must_use]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// quota 视图，向下取整（与 new-api `int()` 截断语义对齐；仅用于展示/导出）。
    #[must_use]
    pub const fn to_quota_floor(self) -> i64 {
        self.0.div_euclid(MICROS_PER_QUOTA)
    }

    /// USD 十进制字符串（如 `"1.5"`、`"0.000002"`），供快照/展示使用。
    #[must_use]
    pub fn to_usd_string(self) -> String {
        format_scaled_1e6(self.0)
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${}", self.to_usd_string())
    }
}

/// 把 scale=1e6 的定点整数格式化为十进制字符串（去尾零）。
/// 同时服务 [`Money`]（micro-USD）与 pricing 的定点倍率。
#[must_use]
pub fn format_scaled_1e6(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs();
    let int_part = abs / 1_000_000;
    let frac_part = abs % 1_000_000;
    if frac_part == 0 {
        return format!("{sign}{int_part}");
    }
    let frac = format!("{frac_part:06}");
    let frac = frac.trim_end_matches('0');
    format!("{sign}{int_part}.{frac}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_view_truncates_toward_negative_infinity() {
        assert_eq!(Money::from_micros(3).to_quota_floor(), 1);
        assert_eq!(Money::from_micros(2).to_quota_floor(), 1);
        assert_eq!(Money::from_micros(1).to_quota_floor(), 0);
        assert_eq!(Money::from_micros(0).to_quota_floor(), 0);
    }

    #[test]
    fn checked_arithmetic_detects_overflow() {
        let max = Money::from_micros(i64::MAX);
        assert_eq!(max.checked_add(Money::from_micros(1)), None);
        assert_eq!(
            Money::from_micros(1).checked_sub(Money::from_micros(3)),
            Some(Money::from_micros(-2))
        );
    }

    #[test]
    fn usd_formatting_trims_trailing_zeros() {
        assert_eq!(Money::from_micros(1_500_000).to_usd_string(), "1.5");
        assert_eq!(Money::from_micros(2).to_usd_string(), "0.000002");
        assert_eq!(Money::from_micros(-90_000).to_usd_string(), "-0.09");
        assert_eq!(Money::from_micros(2_000_000).to_usd_string(), "2");
    }
}
