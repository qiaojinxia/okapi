//! 管理端与自助面的标识符校验，统一一套规则（第十一轮评审）。
//!
//! - **机器标识**（分组 / 池 / 套餐 / 出口组的 code）：会拼进 Redis 键、`mb:blocks` 字段
//!   （`<group>|<channel_id>`）与审计，只收 `[A-Za-z0-9_.-]`，`|`、`:`、空白一律拒。
//! - **显示文本**（渠道名、key 名、套餐名）：trim、按字符计长度（与 PG `varchar(n)` 同口径），
//!   禁控制字符与不可见字符；超长回 400，不让 PG 回 500。
//! - **用户名**：显示文本规则 + NFC 规范化后入库，`é` 的组合 / 预组合两种写法撞同一个唯一键。
//! - **邮箱**：单个 `@`、ASCII dot-atom 本地部分、带点的域名；`:`、空白、控制字符进不来。

use crate::gateway::error::AppError;
use crate::text::is_invisible;
use unicode_normalization::UnicodeNormalization;

/// 机器标识：trim 后 1..=`max` 字节，只含字母、数字与 `_` `.` `-`。
pub fn ensure_code<'a>(param: &'static str, raw: &'a str, max: usize) -> Result<&'a str, AppError> {
    let code = raw.trim();
    let valid = (1..=max).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if valid {
        Ok(code)
    } else {
        Err(AppError::bad_request().with_param(param))
    }
}

fn clean_text(text: &str, max: usize) -> bool {
    text.chars().count() <= max && !text.chars().any(|c| c.is_control() || is_invisible(c))
}

/// 必填的显示文本：trim 后非空、不超过 `max` 个字符、无控制字符与不可见字符。
pub fn ensure_display_name<'a>(
    param: &'static str,
    raw: &'a str,
    max: usize,
) -> Result<&'a str, AppError> {
    let text = raw.trim();
    if !text.is_empty() && clean_text(text, max) {
        Ok(text)
    } else {
        Err(AppError::bad_request().with_param(param))
    }
}

/// 可留空的显示文本（空串表示清空），其余规则同 [`ensure_display_name`]。
pub fn ensure_optional_text<'a>(
    param: &'static str,
    raw: &'a str,
    max: usize,
) -> Result<&'a str, AppError> {
    let text = raw.trim();
    if clean_text(text, max) {
        Ok(text)
    } else {
        Err(AppError::bad_request().with_param(param))
    }
}

/// 用户名：NFC 规范化后按显示文本规则校验（`users.username varchar(64)`），返回入库值。
pub fn normalize_username(raw: &str) -> Result<String, AppError> {
    let name: String = raw.trim().nfc().collect();
    if !name.is_empty() && clean_text(&name, 64) {
        Ok(name)
    } else {
        Err(AppError::bad_request().with_param("username"))
    }
}

/// 邮箱格式（调用方已 trim + 小写）。不追求覆盖 RFC 5322 的全部写法：引号本地部分、
/// IP 字面量域名、非 ASCII 地址都不收（IDN 域名请填 punycode）。
#[must_use]
pub fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    let atom = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{|}~-".contains(&b);
    let local_ok = (1..=64).contains(&local.len())
        && local
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(atom));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    email.len() <= 254 && local_ok && domain_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_reject_separators_whitespace_and_overflow() {
        assert_eq!(ensure_code("c", " vip-1.a_b ", 32).unwrap(), "vip-1.a_b");
        assert!(ensure_code("c", "Default", 32).is_ok());
        for bad in ["", "   ", "a|0|999", "a:b", "a b", "x\nrl:1", "中文", "a/b"] {
            assert!(ensure_code("c", bad, 32).is_err(), "{bad:?}");
        }
        assert!(ensure_code("c", &"a".repeat(32), 32).is_ok());
        assert!(ensure_code("c", &"a".repeat(33), 32).is_err());
        assert!(ensure_code("c", &"a".repeat(64), 64).is_ok());
    }

    #[test]
    fn display_text_counts_characters_and_blocks_invisible_ones() {
        assert_eq!(
            ensure_display_name("n", "  中文渠道 ", 128).unwrap(),
            "中文渠道"
        );
        assert!(ensure_display_name("n", &"中".repeat(128), 128).is_ok());
        assert!(ensure_display_name("n", &"中".repeat(129), 128).is_err());
        assert!(ensure_display_name("n", " ", 128).is_err());
        assert!(ensure_display_name("n", "a\nb", 128).is_err());
        assert!(ensure_display_name("n", "ad\u{200b}min", 128).is_err());
        assert_eq!(ensure_optional_text("n", "  ", 128).unwrap(), "");
        assert!(ensure_optional_text("n", "x\u{202e}y", 128).is_err());
    }

    #[test]
    fn usernames_are_nfc_and_bounded() {
        // e + U+0301 与预组合的 é 入库后是同一个值，唯一键挡得住
        assert_eq!(normalize_username("Re\u{301}my").unwrap(), "R\u{e9}my");
        assert_eq!(normalize_username(" 中文用户 ").unwrap(), "中文用户");
        assert!(normalize_username(&"中".repeat(64)).is_ok());
        assert!(normalize_username(&"中".repeat(65)).is_err());
        assert!(normalize_username("").is_err());
        assert!(normalize_username("name\nspoof").is_err());
        assert!(normalize_username("ad\u{200d}min").is_err());
        assert!(normalize_username("\u{feff}admin").is_err());
    }

    #[test]
    fn emails_need_one_at_a_dot_atom_and_a_dotted_domain() {
        for ok in [
            "a@b.co",
            "first.last+tag@mail.example.com",
            "x_y-z@sub-domain.test",
            "o'neil@ex.io",
        ] {
            assert!(valid_email(ok), "{ok}");
        }
        for bad in [
            "cd:victim@x.test",
            "a@b",
            "a@@b.co",
            "a@b@c.co",
            "@b.co",
            "a@.co",
            "a..b@c.co",
            ".a@c.co",
            "a@-b.co",
            "a b@c.co",
            "a@b.co\n",
            "a\r\n@b.co",
            "用户@例子.中国",
            "a@b_c.co",
        ] {
            assert!(!valid_email(bad), "{bad:?}");
        }
        assert!(!valid_email(&format!("{}@b.co", "a".repeat(65))));
    }
}
