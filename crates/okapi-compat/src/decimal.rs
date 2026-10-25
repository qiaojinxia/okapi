//! JSON 数字的精确十进制比较（`clamp` 用）。
//!
//! 不经过任何浮点：`serde_json` 开了 `arbitrary_precision`，数字保留原文，这里直接按原文的
//! 符号 / 有效数字 / 十进制指数比大小。`1.0` 与 `1`、`1e0` 相等；`0.5` 小于 `1`。

use std::cmp::Ordering;

/// 指数绝对值的上限：再大的数字当作「不可比」，`clamp` 跳过而不是去算一个天文数字。
const MAX_EXPONENT: i64 = 1000;
/// 有效数字位数上限，防畸形输入。
const MAX_DIGITS: usize = 1000;

/// `value = (-1)^neg × 0.d1d2d3… × 10^point`；零用空 `digits` 表示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decimal {
    neg: bool,
    digits: Vec<u8>,
    point: i64,
}

impl Decimal {
    /// 解析 JSON 数字原文；不合法（含超限）返回 `None`。
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (neg, rest) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let (mantissa, exp) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], rest[i + 1..].parse::<i64>().ok()?),
            None => (rest, 0),
        };
        if exp.abs() > MAX_EXPONENT {
            return None;
        }
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((int, frac)) => (int, frac),
            None => (mantissa, ""),
        };
        if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if !frac_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut digits: Vec<u8> = int_part
            .bytes()
            .chain(frac_part.bytes())
            .map(|b| b - b'0')
            .collect();
        if digits.len() > MAX_DIGITS {
            return None;
        }
        let mut point = i64::try_from(int_part.len()).ok()?.checked_add(exp)?;
        let leading = digits.iter().take_while(|&&d| d == 0).count();
        digits.drain(..leading);
        point -= i64::try_from(leading).ok()?;
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            return Some(Self {
                neg: false,
                digits,
                point: 0,
            });
        }
        Some(Self { neg, digits, point })
    }

    fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    fn cmp_magnitude(&self, other: &Self) -> Ordering {
        self.point
            .cmp(&other.point)
            .then_with(|| self.digits.cmp(&other.digits))
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.neg {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if self.neg {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {}
        }
        match (self.neg, other.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        Decimal::parse(text).expect("decimal")
    }

    #[test]
    fn equal_spellings_compare_equal() {
        assert_eq!(d("1"), d("1.0"));
        assert_eq!(d("1"), d("1e0"));
        assert_eq!(d("100"), d("1e2"));
        assert_eq!(d("0.5"), d("5e-1"));
        assert_eq!(d("0"), d("-0"));
        assert_eq!(d("0.000"), d("0"));
        assert_eq!(d("007"), d("7"));
    }

    #[test]
    fn orders_across_signs_and_magnitudes() {
        assert!(d("0.5") < d("1"));
        assert!(d("-1") < d("0.5"));
        assert!(d("-2") < d("-1"));
        assert!(d("-0.5") > d("-1"));
        assert!(d("128") > d("16"));
        assert!(d("2") > d("1.9999999999999999999999"));
        assert!(d("10") > d("9.99"));
        assert!(d("-0.001") < d("0"));
        assert!(d("0") < d("0.001"));
    }

    #[test]
    fn rejects_malformed_and_absurd_numbers() {
        assert_eq!(Decimal::parse(""), None);
        assert_eq!(Decimal::parse("-"), None);
        assert_eq!(Decimal::parse(".5"), None);
        assert_eq!(Decimal::parse("1.2.3"), None);
        assert_eq!(Decimal::parse("abc"), None);
        assert_eq!(Decimal::parse("1e99999"), None);
        assert_eq!(Decimal::parse(&"9".repeat(2000)), None);
    }
}
