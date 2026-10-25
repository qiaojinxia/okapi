//! `usage_map` 的整数表达式：槽位名、非负整数字面量、`+` `-`、括号、`max(a, b)` `min(a, b)`。
//!
//! 刻意做得很小，而且**没有浮点、没有除法、没有乘法**：规则只能把上游报的用量事实重新摆放，
//! 不能凭空造出另一个量级的数——金额永远由价簿用整数引擎算。运算全程 `i128` 饱和，
//! 最终结果夹到 `0..=i64::MAX`（用量不会是负数）。引用的槽位缺失或不是非负整数时求值失败（`None`），
//! 调用方据此整条不写（失败开放）。

use serde_json::Value;
use std::fmt;

pub const MAX_EXPR_LEN: usize = 256;
pub const MAX_EXPR_DEPTH: usize = 8;
pub const MAX_EXPR_NODES: usize = 64;
/// 字面量上限：远大于任何真实的 token 数，又留足 `i128` 里饱和运算的余量。
const MAX_LITERAL: i128 = 1_000_000_000_000_000_000;

/// 用量槽位：OpenAI chat 形状的 `usage` 里账单会读的那几项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    Prompt,
    Completion,
    Total,
    CacheRead,
    Reasoning,
    AudioPrompt,
    AudioCompletion,
}

impl Slot {
    pub const ALL: [Self; 7] = [
        Self::Prompt,
        Self::Completion,
        Self::Total,
        Self::CacheRead,
        Self::Reasoning,
        Self::AudioPrompt,
        Self::AudioCompletion,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Completion => "completion",
            Self::Total => "total",
            Self::CacheRead => "cache_read",
            Self::Reasoning => "reasoning",
            Self::AudioPrompt => "audio_prompt",
            Self::AudioCompletion => "audio_completion",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|slot| slot.name() == name)
    }

    /// 槽位在响应文档里的位置（RFC 6901）。
    #[must_use]
    pub const fn pointer(self) -> &'static str {
        match self {
            Self::Prompt => "/usage/prompt_tokens",
            Self::Completion => "/usage/completion_tokens",
            Self::Total => "/usage/total_tokens",
            Self::CacheRead => "/usage/prompt_tokens_details/cached_tokens",
            Self::Reasoning => "/usage/completion_tokens_details/reasoning_tokens",
            Self::AudioPrompt => "/usage/prompt_tokens_details/audio_tokens",
            Self::AudioCompletion => "/usage/completion_tokens_details/audio_tokens",
        }
    }

    /// 读槽位当前的值：必须是非负整数，浮点（哪怕 `5.0`）、字符串、缺失都算没有。
    #[must_use]
    pub fn read(self, doc: &Value) -> Option<i128> {
        let node = doc.pointer(self.pointer())?;
        match node {
            Value::Number(n) => n
                .as_u64()
                .map(i128::from)
                .or_else(|| n.as_i64().filter(|v| *v >= 0).map(i128::from)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Lit(i128),
    Slot(Slot),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Max(Box<Expr>, Box<Expr>),
    Min(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExprError {
    #[error("expr_too_long")]
    TooLong,
    #[error("expr_too_deep")]
    TooDeep,
    #[error("expr_too_large")]
    TooLarge,
    #[error("unexpected_end")]
    End,
    #[error("unexpected_char `{0}`")]
    Char(char),
    #[error("unknown_slot `{0}`")]
    UnknownSlot(String),
    #[error("literal_too_large")]
    Literal,
    #[error("trailing_input")]
    Trailing,
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    nodes: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.chars.peek().is_some_and(|c| c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn node(&mut self) -> Result<(), ExprError> {
        self.nodes += 1;
        if self.nodes > MAX_EXPR_NODES {
            return Err(ExprError::TooLarge);
        }
        Ok(())
    }

    fn eat(&mut self, want: char) -> Result<(), ExprError> {
        self.skip_ws();
        match self.chars.next() {
            Some(c) if c == want => Ok(()),
            Some(c) => Err(ExprError::Char(c)),
            None => Err(ExprError::End),
        }
    }

    // expr := term (('+' | '-') term)*
    fn expr(&mut self, depth: usize) -> Result<Expr, ExprError> {
        if depth > MAX_EXPR_DEPTH {
            return Err(ExprError::TooDeep);
        }
        let mut left = self.term(depth)?;
        loop {
            self.skip_ws();
            let op = match self.chars.peek() {
                Some('+') => '+',
                Some('-') => '-',
                _ => return Ok(left),
            };
            self.chars.next();
            let right = self.term(depth)?;
            self.node()?;
            left = if op == '+' {
                Expr::Add(Box::new(left), Box::new(right))
            } else {
                Expr::Sub(Box::new(left), Box::new(right))
            };
        }
    }

    // term := NUMBER | NAME | NAME '(' expr ',' expr ')' | '(' expr ')'
    fn term(&mut self, depth: usize) -> Result<Expr, ExprError> {
        self.skip_ws();
        self.node()?;
        match self.chars.peek().copied() {
            None => Err(ExprError::End),
            Some('(') => {
                self.chars.next();
                let inner = self.expr(depth + 1)?;
                self.eat(')')?;
                Ok(inner)
            }
            Some(c) if c.is_ascii_digit() => {
                let mut value: i128 = 0;
                while let Some(d) = self.chars.peek().and_then(|c| c.to_digit(10)) {
                    value = value.saturating_mul(10).saturating_add(i128::from(d));
                    if value > MAX_LITERAL {
                        return Err(ExprError::Literal);
                    }
                    self.chars.next();
                }
                Ok(Expr::Lit(value))
            }
            Some(c) if c.is_ascii_lowercase() || c == '_' => {
                let mut name = String::new();
                while let Some(&c) = self.chars.peek() {
                    if c.is_ascii_lowercase() || c == '_' {
                        name.push(c);
                        self.chars.next();
                    } else {
                        break;
                    }
                }
                self.skip_ws();
                if self.chars.peek() == Some(&'(') && matches!(name.as_str(), "max" | "min") {
                    self.chars.next();
                    let a = self.expr(depth + 1)?;
                    self.eat(',')?;
                    let b = self.expr(depth + 1)?;
                    self.eat(')')?;
                    self.node()?;
                    return Ok(if name == "max" {
                        Expr::Max(Box::new(a), Box::new(b))
                    } else {
                        Expr::Min(Box::new(a), Box::new(b))
                    });
                }
                Slot::parse(&name)
                    .map(Expr::Slot)
                    .ok_or(ExprError::UnknownSlot(name))
            }
            Some(c) => Err(ExprError::Char(c)),
        }
    }
}

impl Expr {
    pub fn parse(text: &str) -> Result<Self, ExprError> {
        if text.len() > MAX_EXPR_LEN {
            return Err(ExprError::TooLong);
        }
        let mut parser = Parser {
            chars: text.chars().peekable(),
            nodes: 0,
        };
        let expr = parser.expr(0)?;
        parser.skip_ws();
        match parser.chars.next() {
            None => Ok(expr),
            Some(_) => Err(ExprError::Trailing),
        }
    }

    /// 求值；引用的槽位读不到（缺失 / 不是非负整数）则整个表达式没有值。
    /// 中间结果允许为负（`max(0, a - b)` 要用），最终夹到 `0..=i64::MAX`。
    #[must_use]
    pub fn eval(&self, doc: &Value) -> Option<i64> {
        let raw = self.eval_raw(doc)?;
        i64::try_from(raw.clamp(0, i128::from(i64::MAX))).ok()
    }

    fn eval_raw(&self, doc: &Value) -> Option<i128> {
        Some(match self {
            Self::Lit(n) => *n,
            Self::Slot(slot) => slot.read(doc)?,
            Self::Add(a, b) => a.eval_raw(doc)?.saturating_add(b.eval_raw(doc)?),
            Self::Sub(a, b) => a.eval_raw(doc)?.saturating_sub(b.eval_raw(doc)?),
            Self::Max(a, b) => a.eval_raw(doc)?.max(b.eval_raw(doc)?),
            Self::Min(a, b) => a.eval_raw(doc)?.min(b.eval_raw(doc)?),
        })
    }

    /// 表达式引用了哪些槽位（诊断 / 蓝图用）。
    #[must_use]
    pub fn slots(&self) -> Vec<Slot> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out.sort();
        out.dedup();
        out
    }

    fn collect(&self, out: &mut Vec<Slot>) {
        match self {
            Self::Lit(_) => {}
            Self::Slot(slot) => out.push(*slot),
            Self::Add(a, b) | Self::Sub(a, b) | Self::Max(a, b) | Self::Min(a, b) => {
                a.collect(out);
                b.collect(out);
            }
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lit(n) => write!(f, "{n}"),
            Self::Slot(slot) => f.write_str(slot.name()),
            Self::Add(a, b) => write!(f, "({a} + {b})"),
            Self::Sub(a, b) => write!(f, "({a} - {b})"),
            Self::Max(a, b) => write!(f, "max({a}, {b})"),
            Self::Min(a, b) => write!(f, "min({a}, {b})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage() -> Value {
        json!({"usage": {
            "prompt_tokens": 100, "completion_tokens": 40, "total_tokens": 190,
            "prompt_tokens_details": {"cached_tokens": 30},
            "completion_tokens_details": {"reasoning_tokens": 50}
        }})
    }

    fn run(text: &str) -> Option<i64> {
        Expr::parse(text).expect("parses").eval(&usage())
    }

    #[test]
    fn parses_and_evaluates_integer_arithmetic() {
        assert_eq!(run("total - prompt"), Some(90));
        assert_eq!(run("completion + reasoning"), Some(90));
        assert_eq!(run("prompt - cache_read"), Some(70));
        assert_eq!(run("max(0, prompt - total)"), Some(0));
        assert_eq!(run("min(prompt, total)"), Some(100));
        assert_eq!(run("(prompt + 1) - (cache_read - 29)"), Some(100));
        assert_eq!(run("  7  "), Some(7));
    }

    #[test]
    fn results_are_clamped_not_negative() {
        assert_eq!(run("prompt - total"), Some(0), "最终结果不为负");
        assert_eq!(
            run("1000000000000000000 + 1000000000000000000"),
            Some(2_000_000_000_000_000_000)
        );
        // 饱和：不会 panic 也不会回绕
        let big = format!(
            "{0} + {0} + {0} + {0} + {0} + {0} + {0} + {0} + {0} + {0}",
            999_999_999_999_999_999_u64
        );
        assert_eq!(
            Expr::parse(&big).expect("parses").eval(&json!({})),
            Some(i64::MAX)
        );
    }

    #[test]
    fn a_missing_or_non_integer_slot_means_no_value() {
        let expr = Expr::parse("total - prompt").expect("parses");
        assert_eq!(expr.eval(&json!({})), None, "没有 usage");
        assert_eq!(
            expr.eval(&json!({"usage": {"total_tokens": 5}})),
            None,
            "缺 prompt"
        );
        for bad in [json!(5.5), json!("5"), json!(-3), json!(null), json!(5.0)] {
            let doc = json!({"usage": {"total_tokens": 9, "prompt_tokens": bad}});
            assert_eq!(expr.eval(&doc), None, "{doc}");
        }
        // 缺失的槽位在 max/min 里也不会被悄悄当作 0
        let guarded = Expr::parse("max(0, total - reasoning)").expect("parses");
        assert_eq!(guarded.eval(&json!({"usage": {"total_tokens": 9}})), None);
    }

    #[test]
    fn syntax_errors_are_specific() {
        for (text, want) in [
            ("", ExprError::End),
            ("total -", ExprError::End),
            ("total prompt", ExprError::Trailing),
            ("total * 2", ExprError::Trailing),
            ("total / 2", ExprError::Trailing),
            ("1.5", ExprError::Trailing),
            ("(total", ExprError::End),
            ("max(1)", ExprError::Char(')')),
            ("tokens", ExprError::UnknownSlot("tokens".into())),
            ("foo(1, 2)", ExprError::UnknownSlot("foo".into())),
            ("1000000000000000001", ExprError::Literal),
            ("$", ExprError::Char('$')),
        ] {
            assert_eq!(Expr::parse(text), Err(want), "{text}");
        }
    }

    #[test]
    fn size_limits_hold() {
        assert_eq!(
            Expr::parse(&"1".repeat(MAX_EXPR_LEN + 1)),
            Err(ExprError::TooLong)
        );
        let deep = format!(
            "{}1{}",
            "(".repeat(MAX_EXPR_DEPTH + 2),
            ")".repeat(MAX_EXPR_DEPTH + 2)
        );
        assert_eq!(Expr::parse(&deep), Err(ExprError::TooDeep));
        let wide = vec!["1"; MAX_EXPR_NODES + 1].join("+");
        assert!(wide.len() <= MAX_EXPR_LEN);
        assert_eq!(Expr::parse(&wide), Err(ExprError::TooLarge));
    }

    #[test]
    fn display_round_trips_through_the_parser() {
        for text in [
            "total - prompt",
            "max(0, (prompt + 1) - cache_read)",
            "min(3, reasoning)",
        ] {
            let expr = Expr::parse(text).expect("parses");
            let again = Expr::parse(&expr.to_string()).expect("re-parses");
            assert_eq!(expr, again, "{text}");
            assert_eq!(expr.eval(&usage()), again.eval(&usage()));
        }
    }

    #[test]
    fn slots_lists_what_an_expression_reads() {
        let expr = Expr::parse("max(0, total - prompt) + cache_read").expect("parses");
        assert_eq!(expr.slots(), [Slot::Prompt, Slot::Total, Slot::CacheRead]);
    }

    #[test]
    fn slot_table_is_consistent() {
        for slot in Slot::ALL {
            assert_eq!(Slot::parse(slot.name()), Some(slot));
            assert!(slot.pointer().starts_with("/usage/"));
        }
    }
}
