//! 选择器里的通配：只有 `*`（任意长度，含空），ASCII 不分大小写。
//!
//! 模型名与主机名都是 ASCII 标识符，不分大小写比较既省掉「`Kimi-K2.6` 对不上 `kimi-k2.6`」
//! 的坑，也不引入正则。双指针回溯，最坏 O(n·m)，模式长度受校验器限制。

#[must_use]
pub fn matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0, 0);
    // 最近一个 `*` 的位置，以及它当前吞到文本的哪里
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p].eq_ignore_ascii_case(&text[t]) {
            p += 1;
            t += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&b| b == b'*')
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn exact_and_case_insensitive() {
        assert!(matches("kimi-k2.6", "Kimi-K2.6"));
        assert!(!matches("kimi-k2.6", "kimi-k2.5"));
        assert!(!matches("kimi", "kimi-k2.6"));
    }

    #[test]
    fn star_matches_any_run_including_empty() {
        assert!(matches("grok-3-mini*", "grok-3-mini"));
        assert!(matches("grok-3-mini*", "grok-3-mini-fast"));
        assert!(!matches("grok-3-mini*", "grok-3"));
        assert!(matches("*.openai.azure.com", "foo.openai.azure.com"));
        assert!(!matches("*.openai.azure.com", "openai.azure.com"));
        assert!(matches("*", ""));
        assert!(matches("a*b*c", "aXXbYYc"));
        assert!(matches("a*b*c", "abc"));
        assert!(!matches("a*b*c", "aXXbYY"));
    }

    #[test]
    fn backtracks_over_repeated_prefixes() {
        assert!(matches("*ab", "aaab"));
        assert!(matches("a*a*a", "aaaaa"));
        assert!(!matches("*abc", "ababab"));
    }

    #[test]
    fn empty_pattern_only_matches_empty_text() {
        assert!(matches("", ""));
        assert!(!matches("", "x"));
    }
}
