//! 与业务无关的文本工具：不可见字符判定、日志转义。

use std::borrow::Cow;
use std::fmt::Write as _;

/// 渲染时不占位、却能改变显示的字符：零宽字符、双向文本控制、软连字符等。
/// 用户名里混进它们，`admin` 与 `ad\u{200b}min` 在列表里肉眼不可分；日志里混进
/// `U+202E` 能让后半行倒着显示。控制字符另由 [`char::is_control`] 判定。
#[must_use]
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// 写进共享日志前的转义：换行、回车写成 `\n` / `\r`，其余控制字符与不可见字符写成 `\u{..}`，
/// 一条日志永远只占一行，也带不进终端转义序列。制表符保留。
#[must_use]
pub fn escape_for_log(s: &str) -> Cow<'_, str> {
    let needs = |c: char| (c.is_control() && c != '\t') || is_invisible(c);
    if !s.chars().any(needs) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if needs(c) => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_text_stays_on_one_line_without_escape_sequences() {
        assert!(matches!(escape_for_log("plain 中文\tok"), Cow::Borrowed(_)));
        assert_eq!(
            escape_for_log("bad\nERROR forged node=gw-1\r"),
            "bad\\nERROR forged node=gw-1\\r"
        );
        assert_eq!(escape_for_log("\u{1b}[31mred"), "\\u{1b}[31mred");
        assert_eq!(
            escape_for_log("a\u{202e}b\u{200b}c"),
            "a\\u{202e}b\\u{200b}c"
        );
    }

    #[test]
    fn invisible_characters_are_recognised() {
        for c in [
            '\u{200b}', '\u{200d}', '\u{feff}', '\u{202e}', '\u{2066}', '\u{00ad}',
        ] {
            assert!(is_invisible(c), "{c:?}");
        }
        for c in ['a', '中', ' ', '-', 'é'] {
            assert!(!is_invisible(c), "{c:?}");
        }
    }
}
