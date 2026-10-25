//! 操作的编译形态与执行。执行只依赖输入的 JSON：无 IO、无时间、无随机，同输入同输出。

use crate::decimal::Decimal;
use crate::pointer::{self, KeepTree, Pointer, Seg};
use serde_json::{Number, Value};
use std::collections::BTreeMap;

/// `clamp` 的一端：原文数字（写回时保持原样拼写）与它的精确十进制。
#[derive(Debug, Clone)]
pub(crate) struct Bound {
    pub number: Number,
    pub decimal: Decimal,
}

#[derive(Debug, Clone)]
pub(crate) enum CompiledOp {
    Remove(Vec<Pointer>),
    KeepOnly(KeepTree),
    Set {
        path: Pointer,
        value: Value,
        if_absent: bool,
    },
    Rename {
        from: Pointer,
        to: Pointer,
        if_absent: bool,
    },
    Copy {
        from: Pointer,
        to: Pointer,
        if_absent: bool,
    },
    Clamp {
        path: Pointer,
        min: Option<Bound>,
        max: Option<Bound>,
    },
    MapValue {
        path: Pointer,
        table: BTreeMap<String, Value>,
    },
    RoleMap {
        targets: Vec<Pointer>,
        from: String,
        to: String,
    },
}

fn clamp_number(number: &Number, min: Option<&Bound>, max: Option<&Bound>) -> Option<Number> {
    let current = Decimal::parse(&number.to_string())?;
    if let Some(min) = min
        && current < min.decimal
    {
        return Some(min.number.clone());
    }
    if let Some(max) = max
        && current > max.decimal
    {
        return Some(max.number.clone());
    }
    None
}

fn replace_string(node: &mut Value, mut next: impl FnMut(&str) -> Option<Value>) -> bool {
    let replacement = match node {
        Value::String(s) => next(s),
        _ => None,
    };
    match replacement {
        Some(value) if value != *node => {
            *node = value;
            true
        }
        _ => false,
    }
}

fn is_top_level(ptr: &Pointer, field: &str) -> bool {
    matches!(ptr.segs(), [Seg::Key(key)] if key == field)
}

impl CompiledOp {
    /// 这个操作是不是把请求顶层的 `field` 接管了（删掉、强制写成某值、整个改名走、
    /// 不在白名单里，或被夹成一个固定值）。控制台试玩据此把渠道已经管着的参数从面板里去掉。
    pub(crate) fn manages_top_level(&self, field: &str) -> bool {
        match self {
            Self::Remove(paths) => paths.iter().any(|p| is_top_level(p, field)),
            Self::KeepOnly(tree) => !tree.keeps_whole(&[field]),
            Self::Set {
                path, if_absent, ..
            } => !if_absent && is_top_level(path, field),
            Self::Rename { from, .. } => is_top_level(from, field),
            Self::Clamp { path, min, max } => {
                is_top_level(path, field)
                    && matches!((min, max), (Some(lo), Some(hi)) if lo.decimal == hi.decimal)
            }
            Self::Copy { .. } | Self::MapValue { .. } | Self::RoleMap { .. } => false,
        }
    }

    /// 应用到 `doc`，返回真正改变的节点个数（0 = 没有任何变化）。
    pub(crate) fn apply(&self, doc: &mut Value) -> usize {
        match self {
            Self::Remove(paths) => paths.iter().map(|p| pointer::remove(doc, p)).sum(),
            Self::KeepOnly(tree) => tree.prune(doc),
            Self::Set {
                path,
                value,
                if_absent,
            } => pointer::set(doc, path, value, *if_absent),
            Self::Rename {
                from,
                to,
                if_absent,
            } => {
                // 目标已有且只许缺失才写：整条不动（来源也留着），与 new-api 对
                // max_completion_tokens 的处理一致
                if *if_absent && pointer::get(doc, to).is_some() {
                    return 0;
                }
                let Some(value) = pointer::get(doc, from).cloned() else {
                    return 0;
                };
                let already_there = pointer::get(doc, to) == Some(&value);
                let placed = pointer::set(doc, to, &value, false);
                if placed == 0 && !already_there {
                    // 放不进去（途经标量等）：来源不动，免得值凭空消失
                    return 0;
                }
                pointer::take(doc, from);
                // 一次移动算一处改动（写入目标 + 删除来源是同一件事）
                1
            }
            Self::Copy {
                from,
                to,
                if_absent,
            } => match pointer::get(doc, from).cloned() {
                Some(value) => pointer::set(doc, to, &value, *if_absent),
                None => 0,
            },
            Self::Clamp { path, min, max } => {
                let mut changed = 0;
                pointer::each_mut(doc, path, &mut |node| {
                    let next = match node {
                        Value::Number(n) => clamp_number(n, min.as_ref(), max.as_ref()),
                        _ => None,
                    };
                    if let Some(number) = next {
                        *node = Value::Number(number);
                        changed += 1;
                    }
                });
                changed
            }
            Self::MapValue { path, table } => {
                let mut changed = 0;
                pointer::each_mut(doc, path, &mut |node| {
                    if replace_string(node, |s| table.get(s).cloned()) {
                        changed += 1;
                    }
                });
                changed
            }
            Self::RoleMap { targets, from, to } => {
                let mut changed = 0;
                for path in targets {
                    pointer::each_mut(doc, path, &mut |node| {
                        let swap =
                            |s: &str| (s == from.as_str()).then(|| Value::String(to.clone()));
                        if replace_string(node, swap) {
                            changed += 1;
                        }
                    });
                }
                changed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(text: &str) -> Pointer {
        Pointer::parse(text).expect("pointer")
    }

    fn bound(text: &str) -> Bound {
        let number: Number = serde_json::from_str(text).expect("number");
        Bound {
            decimal: Decimal::parse(text).expect("decimal"),
            number,
        }
    }

    #[test]
    fn clamp_pulls_values_into_range_and_keeps_the_bound_spelling() {
        let op = CompiledOp::Clamp {
            path: p("/temperature"),
            min: Some(bound("0")),
            max: Some(bound("1.0")),
        };
        let mut doc = json!({"temperature": 1.5});
        assert_eq!(op.apply(&mut doc), 1);
        assert_eq!(
            serde_json::to_string(&doc["temperature"]).expect("json"),
            "1.0"
        );
        assert_eq!(op.apply(&mut doc), 0, "幂等");
        let mut low = json!({"temperature": -3});
        assert_eq!(op.apply(&mut low), 1);
        assert_eq!(low["temperature"], json!(0));
        let mut inside = json!({"temperature": 0.7});
        assert_eq!(op.apply(&mut inside), 0);
        let mut absent = json!({});
        assert_eq!(op.apply(&mut absent), 0, "没有这个字段就不凭空写");
        let mut wrong_type = json!({"temperature": "hot"});
        assert_eq!(op.apply(&mut wrong_type), 0);
    }

    #[test]
    fn clamp_to_a_single_value_pins_only_present_numbers() {
        // new-api：kimi-k2.6 的 temperature 带了且不是 1 才改成 1
        let op = CompiledOp::Clamp {
            path: p("/temperature"),
            min: Some(bound("1")),
            max: Some(bound("1")),
        };
        let mut doc = json!({"temperature": 0.3});
        assert_eq!(op.apply(&mut doc), 1);
        assert_eq!(doc["temperature"], json!(1));
        let mut same = json!({"temperature": 1.0});
        assert_eq!(op.apply(&mut same), 0, "1.0 与 1 相等，不改写");
        let mut absent = json!({"top_p": 1});
        assert_eq!(op.apply(&mut absent), 0);
    }

    #[test]
    fn clamp_lower_bound_keeps_integers_integral() {
        let op = CompiledOp::Clamp {
            path: p("/max_output_tokens"),
            min: Some(bound("128")),
            max: None,
        };
        let mut doc = json!({"max_output_tokens": 16});
        assert_eq!(op.apply(&mut doc), 1);
        assert_eq!(doc["max_output_tokens"], json!(128));
        assert!(doc["max_output_tokens"].is_u64());
    }

    #[test]
    fn rename_moves_and_is_idempotent() {
        let op = CompiledOp::Rename {
            from: p("/max_tokens"),
            to: p("/max_completion_tokens"),
            if_absent: false,
        };
        let mut doc = json!({"max_tokens": 10, "model": "m"});
        assert_eq!(op.apply(&mut doc), 1);
        assert_eq!(doc, json!({"max_completion_tokens": 10, "model": "m"}));
        assert_eq!(op.apply(&mut doc), 0);
        let mut both = json!({"max_tokens": 10, "max_completion_tokens": 20});
        assert_eq!(op.apply(&mut both), 1, "always：覆盖目标");
        assert_eq!(both, json!({"max_completion_tokens": 10}));
    }

    #[test]
    fn rename_if_absent_leaves_everything_alone_when_the_target_exists() {
        let op = CompiledOp::Rename {
            from: p("/max_tokens"),
            to: p("/max_completion_tokens"),
            if_absent: true,
        };
        let mut both = json!({"max_tokens": 10, "max_completion_tokens": 20});
        assert_eq!(op.apply(&mut both), 0);
        assert_eq!(both, json!({"max_tokens": 10, "max_completion_tokens": 20}));
        let mut only_from = json!({"max_tokens": 10});
        assert_eq!(op.apply(&mut only_from), 1);
        assert_eq!(only_from, json!({"max_completion_tokens": 10}));
    }

    #[test]
    fn rename_never_loses_the_value_when_the_target_cannot_be_written() {
        let op = CompiledOp::Rename {
            from: p("/a"),
            to: p("/b/c"),
            if_absent: false,
        };
        let mut doc = json!({"a": 1, "b": "scalar"});
        assert_eq!(op.apply(&mut doc), 0);
        assert_eq!(doc, json!({"a": 1, "b": "scalar"}));
    }

    #[test]
    fn copy_keeps_the_source() {
        let op = CompiledOp::Copy {
            from: p("/a"),
            to: p("/b"),
            if_absent: false,
        };
        let mut doc = json!({"a": {"x": 1}});
        assert_eq!(op.apply(&mut doc), 1);
        assert_eq!(doc, json!({"a": {"x": 1}, "b": {"x": 1}}));
        assert_eq!(op.apply(&mut doc), 0);
    }

    #[test]
    fn map_value_replaces_listed_strings_only() {
        let op = CompiledOp::MapValue {
            path: p("/tool_choice"),
            table: BTreeMap::from([("required".to_owned(), json!("any"))]),
        };
        let mut doc = json!({"tool_choice": "required"});
        assert_eq!(op.apply(&mut doc), 1);
        assert_eq!(doc["tool_choice"], json!("any"));
        assert_eq!(op.apply(&mut doc), 0);
        let mut object = json!({"tool_choice": {"type": "required"}});
        assert_eq!(op.apply(&mut object), 0);
    }

    #[test]
    fn role_map_covers_chat_messages_and_responses_input() {
        let op = CompiledOp::RoleMap {
            targets: vec![p("/messages/*/role"), p("/input/*/role")],
            from: "developer".to_owned(),
            to: "system".to_owned(),
        };
        let mut chat =
            json!({"messages": [{"role": "developer", "content": "x"}, {"role": "user"}]});
        assert_eq!(op.apply(&mut chat), 1);
        assert_eq!(chat["messages"][0]["role"], json!("system"));
        assert_eq!(chat["messages"][1]["role"], json!("user"));
        let mut responses =
            json!({"input": [{"role": "developer"}, {"type": "function_call_output"}]});
        assert_eq!(op.apply(&mut responses), 1);
        assert_eq!(op.apply(&mut responses), 0);
        let mut text_input = json!({"input": "plain"});
        assert_eq!(op.apply(&mut text_input), 0);
    }

    #[test]
    fn set_and_remove_report_what_changed() {
        let set = CompiledOp::Set {
            path: p("/thinking/type"),
            value: json!("disabled"),
            if_absent: false,
        };
        let remove = CompiledOp::Remove(vec![p("/user"), p("/missing")]);
        let mut doc = json!({"user": "u"});
        assert_eq!(set.apply(&mut doc), 1);
        assert_eq!(remove.apply(&mut doc), 1);
        assert_eq!(doc, json!({"thinking": {"type": "disabled"}}));
    }
}
