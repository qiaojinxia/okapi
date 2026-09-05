//! 模型名修饰符（IMPLEMENTATION §11.25，形状对齐 new-api rc.32/33）。
//!
//! 语法：`<base>@<key>:<value>[@<key>:<value>…]`，例如
//! `gpt-5@effort:high`、`claude-x@thinking:4096`、`gpt-5@thinking:on@effort:low`。
//!
//! 为什么要泛化：此前只认四个写死的后缀（`-high/-medium/-low/-thinking[-N]`，
//! 见 [`crate::reasoning`]），既加不了新维度，也表达不了组合，更**没法给变体单独定价**
//! ——`gpt-5@effort:high` 想比基座贵一点是做不到的。
//!
//! 三条语义：
//! 1. **规范名与书写顺序无关**：`a@effort:high@thinking:on` 与 `a@thinking:on@effort:high`
//!    规范化后是同一个计费名，否则同一份配置会因客户端拼接顺序不同而落成两条账。
//! 2. **同键后写覆盖先写**：`a@effort:low@effort:high` = high（与 HTTP 头、查询串同直觉）。
//! 3. **只认识的键才放行**：不认识的键直接判为"这不是修饰符语法"，让模型解析照常走
//!    未命中路径。装作认识、注入不了又照收钱，比报个模型不存在糟得多。
//!
//! 旧的连字符后缀继续可用（`split_reasoning_suffix` 兜底），存量调用方不受影响。

use crate::reasoning::{Effort, ReasoningDirective, split_reasoning_suffix};

/// 修饰符分隔符。选 `@` 而非 `-`：模型名里连字符太常见，`-high` 那套天生会跟
/// 真实模型名（如 `o3-high`）撞车，只能靠"全名直命中优先"打补丁。
const SEP: char = '@';

/// 一次解析的结果：基座模型名 + 规范化后的修饰符。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelModifiers {
    base: String,
    /// 已按键升序排序、同键取最后一个。
    entries: Vec<(String, String)>,
}

impl ModelModifiers {
    /// 规范计费名：`base@k1:v1@k2:v2`（键升序）。无修饰符时即基座名。
    #[must_use]
    pub fn canonical_name(&self) -> String {
        let mut out = self.base.clone();
        for (k, v) in &self.entries {
            out.push(SEP);
            out.push_str(k);
            out.push(':');
            out.push_str(v);
        }
        out
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// 折成既有的 reasoning 指令，复用 [`crate::reasoning`] 已有的三向注入。
    #[must_use]
    pub fn reasoning(&self) -> Option<ReasoningDirective> {
        let mut effort = None;
        let mut budget_tokens = None;
        let mut touched = false;
        for (k, v) in &self.entries {
            match k.as_str() {
                "effort" => {
                    effort = Effort::parse(v);
                    touched = true;
                }
                "thinking" => {
                    touched = true;
                    // on = 用缺省预算（由 ReasoningDirective::effective_budget 决定）；
                    // 数字 = 显式预算；off = 不开思考，直接不产生指令
                    match v.as_str() {
                        "off" | "false" | "0" => return None,
                        "on" | "true" => budget_tokens = None,
                        n => budget_tokens = n.parse::<u32>().ok(),
                    }
                }
                _ => {}
            }
        }
        touched.then_some(ReasoningDirective {
            effort,
            budget_tokens,
        })
    }
}

/// 键是否是我们认识并且能注入的。不认识就不假装认识——见模块文档第 3 条。
fn known_key(key: &str) -> bool {
    matches!(key, "effort" | "thinking")
}

/// 值是否合法（认识的键才有资格谈值域）。
fn valid_value(key: &str, value: &str) -> bool {
    match key {
        "effort" => Effort::parse(value).is_some(),
        "thinking" => {
            matches!(value, "on" | "off" | "true" | "false")
                || value.parse::<u32>().is_ok_and(|n| n > 0)
        }
        _ => false,
    }
}

/// 解析 `base@k:v@k:v`。不含 `@` 时回落到旧的连字符后缀语法。
///
/// 任一段不合法（缺冒号、键不认识、值越界、基座为空）→ 返回 None，
/// 交由调用方按"这个模型名不存在"处理。
#[must_use]
pub fn split_model_modifiers(name: &str) -> Option<ModelModifiers> {
    if !name.contains(SEP) {
        // 旧语法：`gpt-x-high` / `claude-y-thinking-4096`
        let (base, directive) = split_reasoning_suffix(name)?;
        return Some(ModelModifiers {
            base: base.to_owned(),
            entries: legacy_entries(&directive),
        });
    }
    let mut parts = name.split(SEP);
    let base = parts.next()?.trim();
    if base.is_empty() {
        return None;
    }
    let mut entries: Vec<(String, String)> = Vec::new();
    for seg in parts {
        let (k, v) = seg.split_once(':')?;
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() || v.is_empty() || !known_key(k) || !valid_value(k, v) {
            return None;
        }
        // 同键后写覆盖先写
        entries.retain(|(ek, _)| ek != k);
        entries.push((k.to_owned(), v.to_owned()));
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Some(ModelModifiers {
        base: base.to_owned(),
        entries,
    })
}

/// 旧后缀 → 等价的修饰符表示，让两种写法落到同一个规范计费名。
fn legacy_entries(d: &ReasoningDirective) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    if let Some(e) = d.effort {
        entries.push(("effort".to_owned(), e.as_str().to_owned()));
    }
    match d.budget_tokens {
        Some(n) => entries.push(("thinking".to_owned(), n.to_string())),
        // `-thinking` 无数字 = 开启思考、用缺省预算
        None if d.effort.is_none() => entries.push(("thinking".to_owned(), "on".to_owned())),
        None => {}
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(name: &str) -> Option<String> {
        split_model_modifiers(name).map(|m| m.canonical_name())
    }

    #[test]
    fn canonical_name_is_order_independent() {
        // 同一份意图被客户端拼成不同顺序，必须落成同一条计费名——
        // 否则同一个变体会分裂成两行账，价也要配两遍
        assert_eq!(
            canon("gpt-5@effort:high@thinking:on"),
            canon("gpt-5@thinking:on@effort:high")
        );
        assert_eq!(
            canon("gpt-5@thinking:on@effort:high").as_deref(),
            Some("gpt-5@effort:high@thinking:on")
        );
    }

    #[test]
    fn later_wins_for_repeated_keys() {
        assert_eq!(
            canon("m@effort:low@effort:high").as_deref(),
            Some("m@effort:high")
        );
    }

    #[test]
    fn unknown_or_malformed_is_not_modifier_syntax() {
        // 不认识的键：宁可判成"模型不存在"，也不要注入不了却照收钱
        assert_eq!(canon("m@wat:1"), None);
        assert_eq!(canon("m@effort"), None, "缺冒号");
        assert_eq!(canon("m@effort:turbo"), None, "值不在值域内");
        assert_eq!(canon("m@thinking:0"), None, "预算 0 无意义");
        assert_eq!(canon("@effort:high"), None, "基座为空");
        assert_eq!(canon("plain-model"), None, "无修饰符也无旧后缀");
    }

    #[test]
    fn legacy_suffix_maps_onto_the_same_canonical_name() {
        // 老客户端发 `-high`，新客户端发 `@effort:high`，两者应落同一条账
        assert_eq!(canon("gpt-5-high").as_deref(), Some("gpt-5@effort:high"));
        assert_eq!(
            canon("claude-x-thinking-4096").as_deref(),
            Some("claude-x@thinking:4096")
        );
        assert_eq!(
            canon("claude-x-thinking").as_deref(),
            Some("claude-x@thinking:on")
        );
    }

    #[test]
    fn reasoning_directive_is_derived_for_injection() {
        let m = split_model_modifiers("gpt-5@effort:high").unwrap();
        assert_eq!(m.base(), "gpt-5");
        assert_eq!(m.reasoning().unwrap().effort, Some(Effort::High));

        let m = split_model_modifiers("c@thinking:4096").unwrap();
        assert_eq!(m.reasoning().unwrap().budget_tokens, Some(4096));

        // thinking:off = 明确不开思考：不产生指令，等于按基座跑
        let m = split_model_modifiers("c@thinking:off").unwrap();
        assert!(m.reasoning().is_none());
        assert_eq!(
            m.canonical_name(),
            "c@thinking:off",
            "但计费名仍记录了这个选择"
        );
    }
}
