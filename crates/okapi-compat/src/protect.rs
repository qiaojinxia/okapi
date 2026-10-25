//! 受保护路径的唯一注册表。
//!
//! 此前这份清单有两个副本（控制台 `inject_request_fields_ok` 与网关 `adjust_request_fields`
//! 的 `STRIP_PROTECTED` / `INJECT_PROTECTED`），靠注释约定「同一份」。现在校验（写入时）与执行
//! （运行期的后置断言）都读这里。
//!
//! 请求体与响应体各有一张表（`classify_in` 按阶段选）；请求那张还管运行期的后置断言。
//!
//! 四个等级，由低到高：
//! - [`PathClass::Free`]：随便写。
//! - [`PathClass::Sensitive`]：写入需要更高权限并留审计（提示词、指令、`service_tier`…）。
//!   **移除不算**——删字段只会让请求更干净，不会注入内容或抬高成本。
//! - [`PathClass::BillingCritical`]：动它会让结算落到字符估算（`stream_options.include_usage`），
//!   规则必须带 `billing_critical_ack`。
//! - [`PathClass::Protected`]：任何规则都不可写。

use crate::pointer::{KeepTree, Pointer, Seg};
use crate::rule::Phase;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathClass {
    Free,
    Sensitive,
    BillingCritical,
    Protected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// 删除（`remove`、`keep_only` 修剪掉的部分、`rename` 的来源）。
    Remove,
    /// 写入（`set`、`rename` / `copy` 的目标、`clamp`、`map_value`、`role_map`）。
    Write,
}

struct Entry {
    /// 键序列；片段 `*` 表示「任意数组下标 / 对象键」。
    path: &'static [&'static str],
    class: PathClass,
    /// 只保护节点本身（不能删 / 整个替换），不保护它的后代。
    node_only: bool,
}

const fn entry(path: &'static [&'static str], class: PathClass) -> Entry {
    Entry {
        path,
        class,
        node_only: false,
    }
}

/// 请求体顶层的受保护键：主链、会话续接与 okapi 自己的路由指令。
/// 运行期后置断言就盯着这几个值有没有被改。
pub const PROTECTED_KEYS: [&str; 5] = [
    "model",
    "stream",
    "previous_response_id",
    "conversation",
    "provider",
];

/// 请求体的注册表（发往上游的那份 JSON）。
const REGISTRY: &[Entry] = &[
    // 主链：模型名只能经渠道的模型映射改；流式开关与会话续接牵动协议与结算。
    entry(&["model"], PathClass::Protected),
    entry(&["stream"], PathClass::Protected),
    entry(&["previous_response_id"], PathClass::Protected),
    entry(&["conversation"], PathClass::Protected),
    // okapi 自己的路由指令：转发前已剥掉，规则再写回去上游会 400。
    entry(&["provider"], PathClass::Protected),
    // 对话本体：整个数组不能删 / 替换；元素内部的字段属敏感，见下。
    Entry {
        path: &["messages"],
        class: PathClass::Protected,
        node_only: true,
    },
    // 流式用量：缺了它结算只能落字符估算，实测漏收约七成（openai.rs `ensure_stream_usage`）。
    entry(
        &["stream_options", "include_usage"],
        PathClass::BillingCritical,
    ),
    // 敏感：提示词、指令、推理与档位。
    entry(&["messages"], PathClass::Sensitive),
    entry(&["input"], PathClass::Sensitive),
    entry(&["system"], PathClass::Sensitive),
    entry(&["instructions"], PathClass::Sensitive),
    entry(&["contents"], PathClass::Sensitive),
    entry(&["reasoning"], PathClass::Sensitive),
    entry(&["reasoning_effort"], PathClass::Sensitive),
    entry(&["service_tier"], PathClass::Sensitive),
];

/// 响应体的注册表（OpenAI chat 形状）。响应里没有「受保护」项：没有主链字段需要钉住，
/// 也就没有运行期快照还原；要管的是两件事——
/// - **用量事实**（`usage`、`bill_by_response_model` 渠道按它计价的 `model`、档位倍率用的 `service_tier`）
///   是计费关键：动它就是动账，规则要 `billing_critical_ack`，写入还需要 `compat.sensitive`；
/// - **模型输出本体**（正文、工具调用、拒答）写入需要 `compat.sensitive`，移除不需要。
const RESPONSE_REGISTRY: &[Entry] = &[
    entry(&["usage"], PathClass::BillingCritical),
    entry(&["model"], PathClass::BillingCritical),
    entry(&["service_tier"], PathClass::BillingCritical),
    entry(
        &["choices", "*", "message", "content"],
        PathClass::Sensitive,
    ),
    entry(
        &["choices", "*", "message", "tool_calls"],
        PathClass::Sensitive,
    ),
    entry(
        &["choices", "*", "message", "function_call"],
        PathClass::Sensitive,
    ),
    entry(
        &["choices", "*", "message", "refusal"],
        PathClass::Sensitive,
    ),
    entry(&["choices", "*", "delta", "content"], PathClass::Sensitive),
    entry(
        &["choices", "*", "delta", "tool_calls"],
        PathClass::Sensitive,
    ),
    entry(
        &["choices", "*", "delta", "function_call"],
        PathClass::Sensitive,
    ),
    entry(&["choices", "*", "delta", "refusal"], PathClass::Sensitive),
];

fn registry(phase: Phase) -> &'static [Entry] {
    match phase {
        Phase::Response => RESPONSE_REGISTRY,
        _ => REGISTRY,
    }
}

/// 注册表的一行，给目录 / 界面看的形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    /// RFC 6901 指针；`*` 表示任意数组下标 / 对象键。
    pub path: String,
    pub class: PathClass,
    /// 只保护节点本身（不能删 / 整个替换），不保护它的后代。
    pub node_only: bool,
}

/// 某个阶段用的注册表（响应看响应那张，其余看请求那张）。
#[must_use]
pub fn describe(phase: Phase) -> Vec<EntryInfo> {
    registry(phase)
        .iter()
        .map(|e| EntryInfo {
            path: Pointer::from_keys(e.path.iter().copied()),
            class: e.class,
            node_only: e.node_only,
        })
        .collect()
}

/// `keep_only` 永远保留的路径：注册表里等级为 Protected 的项（含 `/messages` 这种只保护节点本身的）。
#[must_use]
pub fn implicit_keep() -> Vec<Pointer> {
    REGISTRY
        .iter()
        .filter(|e| e.class == PathClass::Protected)
        .filter_map(|e| Pointer::parse(&Pointer::from_keys(e.path.iter().copied())).ok())
        .collect()
}

/// 写入 `ptr` 会不会碰到注册表里的某一项：相等、祖先（整棵换掉会连带改到它）、
/// 或后代（`node_only` 的项不保护后代）。通配片段当作可能等于任何键。
fn overlaps(written: &[Seg], entry: &Entry) -> bool {
    let common = written.len().min(entry.path.len());
    for (seg, key) in written.iter().zip(entry.path).take(common) {
        if let Seg::Key(k) = seg
            && k != key
            && *key != "*"
        {
            return false;
        }
    }
    written.len() <= entry.path.len() || !entry.node_only
}

/// 这条指针在给定访问方式下的最高等级（请求体）。
#[must_use]
pub fn classify(ptr: &Pointer, access: Access) -> PathClass {
    classify_in(Phase::Request, ptr, access)
}

/// 同 [`classify`]，按阶段选注册表：`response` 看响应体那张，其余看请求体那张。
#[must_use]
pub fn classify_in(phase: Phase, ptr: &Pointer, access: Access) -> PathClass {
    registry(phase)
        .iter()
        .filter(|e| overlaps(ptr.segs(), e))
        .map(|e| e.class)
        .filter(|class| access == Access::Write || *class != PathClass::Sensitive)
        .max()
        .unwrap_or(PathClass::Free)
}

/// 给 `set` 用：写成 `include_usage: true`（或写一个带它的 `stream_options` 对象）是在**加固**
/// 计费，不需要确认。其余动到这条路径的写法都算计费关键。
#[must_use]
pub fn billing_safe_set(ptr: &Pointer, value: &Value) -> bool {
    let keys: Vec<&str> = ptr
        .segs()
        .iter()
        .filter_map(|seg| match seg {
            Seg::Key(k) => Some(k.as_str()),
            Seg::Any => None,
        })
        .collect();
    if keys.len() != ptr.segs().len() {
        return false;
    }
    match keys.as_slice() {
        ["stream_options", "include_usage"] => value == &Value::Bool(true),
        ["stream_options"] => value
            .get("include_usage")
            .is_some_and(|v| v == &Value::Bool(true)),
        _ => false,
    }
}

/// `keep_only` 白名单没有覆盖到的计费关键项（会被修剪掉）。
#[must_use]
pub fn billing_critical_dropped(keep: &KeepTree) -> bool {
    REGISTRY
        .iter()
        .filter(|e| e.class == PathClass::BillingCritical)
        .any(|e| !keep.keeps_whole(e.path))
}

/// 运行期后置断言用的快照：规则应用前后，受保护键的值必须一致。
pub struct Snapshot(Vec<(&'static str, Option<Value>)>);

impl Snapshot {
    #[must_use]
    pub fn capture(doc: &Value) -> Self {
        Self(
            PROTECTED_KEYS
                .iter()
                .map(|key| (*key, doc.get(*key).cloned()))
                .collect(),
        )
    }

    #[must_use]
    pub fn intact(&self, doc: &Value) -> bool {
        self.0
            .iter()
            .all(|(key, value)| doc.get(*key) == value.as_ref())
    }

    /// 把被改动的受保护键恢复原样。
    pub fn restore(&self, doc: &mut Value) {
        let Some(obj) = doc.as_object_mut() else {
            return;
        };
        for (key, value) in &self.0 {
            match value {
                Some(v) => {
                    obj.insert((*key).to_owned(), v.clone());
                }
                None => {
                    obj.remove(*key);
                }
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

    #[test]
    fn trunk_fields_are_protected_for_every_access() {
        for key in PROTECTED_KEYS {
            let ptr = p(&format!("/{key}"));
            assert_eq!(classify(&ptr, Access::Write), PathClass::Protected, "{key}");
            assert_eq!(
                classify(&ptr, Access::Remove),
                PathClass::Protected,
                "{key}"
            );
        }
    }

    #[test]
    fn registry_covers_every_protected_key() {
        for key in PROTECTED_KEYS {
            assert!(
                REGISTRY
                    .iter()
                    .any(|e| e.class == PathClass::Protected && e.path == [key]),
                "{key} 没有登记进注册表"
            );
        }
    }

    #[test]
    fn messages_array_is_protected_but_its_elements_are_only_sensitive() {
        assert_eq!(
            classify(&p("/messages"), Access::Remove),
            PathClass::Protected
        );
        assert_eq!(
            classify(&p("/messages"), Access::Write),
            PathClass::Protected
        );
        assert_eq!(
            classify(&p("/messages/*/role"), Access::Write),
            PathClass::Sensitive
        );
        assert_eq!(
            classify(&p("/messages/0/content"), Access::Write),
            PathClass::Sensitive
        );
        assert_eq!(
            classify(&p("/messages/*/role"), Access::Remove),
            PathClass::Free
        );
    }

    #[test]
    fn removing_sensitive_fields_is_free_but_writing_them_is_not() {
        for key in [
            "instructions",
            "system",
            "service_tier",
            "reasoning",
            "input",
            "contents",
        ] {
            let ptr = p(&format!("/{key}"));
            assert_eq!(classify(&ptr, Access::Remove), PathClass::Free, "{key}");
            assert_eq!(classify(&ptr, Access::Write), PathClass::Sensitive, "{key}");
        }
        assert_eq!(
            classify(&p("/reasoning/effort"), Access::Write),
            PathClass::Sensitive
        );
    }

    #[test]
    fn stream_usage_is_billing_critical_for_itself_and_its_ancestor() {
        for path in ["/stream_options/include_usage", "/stream_options"] {
            assert_eq!(
                classify(&p(path), Access::Remove),
                PathClass::BillingCritical,
                "{path}"
            );
            assert_eq!(
                classify(&p(path), Access::Write),
                PathClass::BillingCritical,
                "{path}"
            );
        }
        assert_eq!(
            classify(&p("/stream_options/other"), Access::Write),
            PathClass::Free
        );
        assert_eq!(
            classify(&p("/stream_options/*"), Access::Remove),
            PathClass::BillingCritical
        );
    }

    #[test]
    fn wildcards_may_hit_a_protected_neighbour() {
        // `/*` 可能就是 /model
        assert_eq!(classify(&p("/*"), Access::Write), PathClass::Protected);
        assert_eq!(classify(&p("/*/x"), Access::Write), PathClass::Protected);
        assert_eq!(classify(&p("/temperature"), Access::Write), PathClass::Free);
    }

    #[test]
    fn descendants_of_a_protected_key_are_protected() {
        assert_eq!(
            classify(&p("/conversation/id"), Access::Write),
            PathClass::Protected
        );
        assert_eq!(
            classify(&p("/model/x"), Access::Remove),
            PathClass::Protected
        );
    }

    #[test]
    fn only_hardening_writes_to_stream_usage_skip_the_ack() {
        assert!(billing_safe_set(
            &p("/stream_options/include_usage"),
            &json!(true)
        ));
        assert!(!billing_safe_set(
            &p("/stream_options/include_usage"),
            &json!(false)
        ));
        assert!(billing_safe_set(
            &p("/stream_options"),
            &json!({"include_usage": true})
        ));
        assert!(!billing_safe_set(&p("/stream_options"), &json!({})));
        assert!(!billing_safe_set(&p("/stream_options"), &json!(null)));
        assert!(!billing_safe_set(&p("/temperature"), &json!(true)));
        assert!(!billing_safe_set(&p("/*/include_usage"), &json!(true)));
    }

    #[test]
    fn keep_only_must_list_stream_usage_to_avoid_the_ack() {
        let keeps = |paths: &[&str]| {
            let ptrs: Vec<Pointer> = paths.iter().map(|s| p(s)).collect();
            billing_critical_dropped(&KeepTree::build(&ptrs))
        };
        assert!(keeps(&["/model", "/messages"]));
        assert!(!keeps(&["/model", "/stream_options"]));
        assert!(!keeps(&["/stream_options/include_usage"]));
        assert!(keeps(&["/stream_options/other"]));
    }

    #[test]
    fn usage_facts_are_billing_critical_in_responses_only() {
        for path in [
            "/usage",
            "/usage/prompt_tokens",
            "/usage/prompt_tokens_details/cached_tokens",
            "/model",
            "/service_tier",
        ] {
            for access in [Access::Write, Access::Remove] {
                assert_eq!(
                    classify_in(Phase::Response, &p(path), access),
                    PathClass::BillingCritical,
                    "{path} {access:?}"
                );
            }
        }
        // 同一个键在请求体里是另一回事
        assert_eq!(classify(&p("/usage"), Access::Write), PathClass::Free);
        assert_eq!(classify(&p("/model"), Access::Write), PathClass::Protected);
        // 通配可能落在 usage 上
        assert_eq!(
            classify_in(Phase::Response, &p("/*/prompt_tokens"), Access::Write),
            PathClass::BillingCritical
        );
    }

    #[test]
    fn model_output_is_sensitive_to_write_but_free_to_remove() {
        for path in [
            "/choices/0/message/content",
            "/choices/*/message/tool_calls",
            "/choices/*/delta/content",
            "/choices/1/delta/refusal",
            "/choices/*/message/function_call",
        ] {
            assert_eq!(
                classify_in(Phase::Response, &p(path), Access::Write),
                PathClass::Sensitive,
                "{path}"
            );
            assert_eq!(
                classify_in(Phase::Response, &p(path), Access::Remove),
                PathClass::Free,
                "{path}"
            );
        }
        // 抄 / 改名推理字段、改 finish_reason 都是自由的
        for path in [
            "/choices/*/message/reasoning_content",
            "/choices/*/delta/reasoning",
            "/choices/*/finish_reason",
            "/id",
            "/system_fingerprint",
        ] {
            assert_eq!(
                classify_in(Phase::Response, &p(path), Access::Write),
                PathClass::Free,
                "{path}"
            );
        }
        // 整个 message / delta 被换掉会连带改到正文
        assert_eq!(
            classify_in(Phase::Response, &p("/choices/*/message"), Access::Write),
            PathClass::Sensitive
        );
        assert_eq!(
            classify_in(Phase::Response, &p("/choices"), Access::Write),
            PathClass::Sensitive
        );
    }

    #[test]
    fn snapshot_detects_and_restores_changes_to_trunk_fields() {
        let mut doc = json!({"model": "m", "stream": true, "x": 1});
        let snap = Snapshot::capture(&doc);
        assert!(snap.intact(&doc));
        doc["model"] = json!("other");
        doc["provider"] = json!({"only": ["a"]});
        doc.as_object_mut().map(|o| o.remove("stream"));
        assert!(!snap.intact(&doc));
        snap.restore(&mut doc);
        assert_eq!(doc, json!({"model": "m", "stream": true, "x": 1}));
        assert!(snap.intact(&doc));
    }
}
