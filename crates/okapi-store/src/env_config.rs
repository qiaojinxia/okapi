//! 环境变量读取的统一口径：写错不悄悄回落。
//!
//! 布尔大小写不敏感（`true/1/yes/on`、`false/0/no/off`）；数值解析失败或越界时打一条 WARN 再用缺省值——
//! 例如 `OKAPI_PG_POOL` 写错一个字符就静默回到 16，现象是 acquire 超时而不是"配置错了"，最难归因。

use std::str::FromStr;

/// 未设置 → `None`；认得的布尔 → `Some`；认不得 → WARN 后 `None`（由调用方取缺省）。
#[must_use]
pub fn flag(name: &str) -> Option<bool> {
    let raw = std::env::var(name).ok()?;
    let parsed = parse_flag(&raw);
    if parsed.is_none() {
        tracing::warn!(name, value = %raw, "unrecognized boolean; using the default");
    }
    parsed
}

fn parse_flag(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// 未设置 → `default`；解析成功且 `valid` → 该值；否则 WARN 后 `default`。
pub fn number<T>(name: &str, default: T, valid: impl Fn(&T) -> bool) -> T
where
    T: FromStr + Copy + std::fmt::Display,
{
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    parse_number(&raw, &valid).unwrap_or_else(|| {
        tracing::warn!(name, value = %raw, %default, "invalid number; using the default");
        default
    })
}

fn parse_number<T: FromStr>(raw: &str, valid: impl Fn(&T) -> bool) -> Option<T> {
    raw.trim().parse::<T>().ok().filter(|value| valid(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_case_insensitive_and_reject_unknown_words() {
        for raw in ["true", "TRUE", " On ", "1", "yes"] {
            assert_eq!(parse_flag(raw), Some(true), "{raw}");
        }
        for raw in ["false", "FALSE", "False", "off", "0", "no"] {
            assert_eq!(parse_flag(raw), Some(false), "{raw}");
        }
        assert_eq!(parse_flag("ture"), None);
    }

    #[test]
    fn numbers_must_parse_and_pass_validation() {
        assert_eq!(parse_number::<u32>(" 32 ", |v| *v > 0), Some(32));
        assert_eq!(parse_number::<u32>("0", |v| *v > 0), None);
        assert_eq!(parse_number::<u32>("16x", |v| *v > 0), None);
    }
}
