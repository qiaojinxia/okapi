//! 定点倍率：scale = 1e6（对应 DB NUMERIC(12,6)）。计费路径禁浮点的核心载体。

use okapi_domain::money::format_scaled_1e6;
use std::str::FromStr;

/// 定点 scale：1.0 = 1_000_000。
pub const RATIO_SCALE: i64 = 1_000_000;

/// 非负定点倍率（模型倍率/补全倍率/缓存倍率/分组倍率/个人倍率/规则乘数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RatioFp(i64);

impl RatioFp {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(RATIO_SCALE);

    /// 从定点原始值构造（负数拒绝）。
    #[must_use]
    pub const fn from_scaled(scaled: i64) -> Option<Self> {
        if scaled < 0 { None } else { Some(Self(scaled)) }
    }

    #[must_use]
    pub const fn as_scaled(self) -> i64 {
        self.0
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for RatioFp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format_scaled_1e6(self.0))
    }
}

/// 十进制字面量解析错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RatioParseError {
    #[error("ratio_parse_empty")]
    Empty,
    #[error("ratio_parse_invalid_char")]
    InvalidChar,
    #[error("ratio_parse_negative")]
    Negative,
    #[error("ratio_parse_too_precise")]
    TooPrecise,
    #[error("ratio_parse_out_of_range")]
    OutOfRange,
}

/// 解析非负十进制字面量（最多 6 位小数）为 scale=1e6 定点整数。
/// 纯整数实现，不经过浮点。
pub fn parse_scaled_1e6(input: &str) -> Result<i64, RatioParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(RatioParseError::Empty);
    }
    if s.starts_with('-') {
        return Err(RatioParseError::Negative);
    }
    let s = s.strip_prefix('+').unwrap_or(s);
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(RatioParseError::Empty);
    }
    if frac_part.len() > 6 {
        return Err(RatioParseError::TooPrecise);
    }

    let mut value: i64 = 0;
    for ch in int_part.chars() {
        let digit = ch.to_digit(10).ok_or(RatioParseError::InvalidChar)?;
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(i64::from(digit)))
            .ok_or(RatioParseError::OutOfRange)?;
    }
    value = value
        .checked_mul(RATIO_SCALE)
        .ok_or(RatioParseError::OutOfRange)?;

    let mut frac: i64 = 0;
    for ch in frac_part.chars() {
        let digit = ch.to_digit(10).ok_or(RatioParseError::InvalidChar)?;
        frac = frac * 10 + i64::from(digit);
    }
    for _ in frac_part.len()..6 {
        frac *= 10;
    }

    value.checked_add(frac).ok_or(RatioParseError::OutOfRange)
}

impl FromStr for RatioFp {
    type Err = RatioParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_scaled_1e6(s).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_literals_exactly() {
        let cases = [
            ("1", 1_000_000),
            ("1.25", 1_250_000),
            ("0.5", 500_000),
            (".5", 500_000),
            ("4", 4_000_000),
            ("0.000002", 2),
            ("0", 0),
            ("1000.123456", 1_000_123_456),
        ];
        for (input, want) in cases {
            assert_eq!(
                input.parse::<RatioFp>().map(RatioFp::as_scaled),
                Ok(want),
                "input={input}"
            );
        }
    }

    #[test]
    fn rejects_invalid_literals() {
        assert_eq!("".parse::<RatioFp>(), Err(RatioParseError::Empty));
        assert_eq!(".".parse::<RatioFp>(), Err(RatioParseError::Empty));
        assert_eq!("-1".parse::<RatioFp>(), Err(RatioParseError::Negative));
        assert_eq!(
            "1.2345678".parse::<RatioFp>(),
            Err(RatioParseError::TooPrecise)
        );
        assert_eq!("1a".parse::<RatioFp>(), Err(RatioParseError::InvalidChar));
    }

    #[test]
    fn display_roundtrip() {
        for literal in ["1.25", "0.9", "2", "0.000002"] {
            assert_eq!(
                literal.parse::<RatioFp>().map(|ratio| ratio.to_string()),
                Ok(literal.to_owned())
            );
        }
    }
}
