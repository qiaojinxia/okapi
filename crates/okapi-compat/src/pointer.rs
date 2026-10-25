//! RFC 6901 JSON Pointer，加一个扩展：整段恰为 `*` 的片段是通配（数组任一元素 / 对象任一成员）。
//!
//! 字面键 `*` 因此没有写法——适配的对象是各家 LLM 的请求体，没有哪个把 `*` 当字段名。
//! 数组下标按 RFC 6901 写成十进制数字片段（`/messages/0/role`），不支持 `-`（追加）：
//! 规则只改已有的东西，不凭空长出数组元素。

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;

pub const MAX_DEPTH: usize = 16;
pub const MAX_SEGMENT_LEN: usize = 128;
pub const MAX_POINTER_LEN: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Seg {
    Key(String),
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pointer {
    segs: Vec<Seg>,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PointerError {
    #[error("root_not_allowed")]
    Root,
    #[error("missing_leading_slash")]
    NoLeadingSlash,
    #[error("bad_escape")]
    BadEscape,
    #[error("empty_segment")]
    EmptySegment,
    #[error("too_deep")]
    TooDeep,
    #[error("segment_too_long")]
    SegmentTooLong,
    #[error("pointer_too_long")]
    TooLong,
}

impl Pointer {
    pub fn parse(text: &str) -> Result<Self, PointerError> {
        if text.is_empty() {
            return Err(PointerError::Root);
        }
        if text.len() > MAX_POINTER_LEN {
            return Err(PointerError::TooLong);
        }
        let rest = text.strip_prefix('/').ok_or(PointerError::NoLeadingSlash)?;
        let mut segs = Vec::new();
        for raw in rest.split('/') {
            if raw.is_empty() {
                return Err(PointerError::EmptySegment);
            }
            if segs.len() == MAX_DEPTH {
                return Err(PointerError::TooDeep);
            }
            if raw == "*" {
                segs.push(Seg::Any);
                continue;
            }
            let key = unescape(raw)?;
            if key.chars().count() > MAX_SEGMENT_LEN {
                return Err(PointerError::SegmentTooLong);
            }
            segs.push(Seg::Key(key));
        }
        Ok(Self {
            segs,
            text: text.to_owned(),
        })
    }

    /// 由字面键拼指针（键里的 `~` 与 `/` 会转义；`*` 键没有写法，调用方不要传）。
    #[must_use]
    pub fn from_keys<'a>(keys: impl IntoIterator<Item = &'a str>) -> String {
        keys.into_iter().fold(String::new(), |mut out, key| {
            out.push('/');
            out.push_str(&escape(key));
            out
        })
    }

    #[must_use]
    pub fn segs(&self) -> &[Seg] {
        &self.segs
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn has_wildcard(&self) -> bool {
        self.segs.contains(&Seg::Any)
    }

    #[must_use]
    pub fn leaf_is_wildcard(&self) -> bool {
        self.segs.last() == Some(&Seg::Any)
    }

    /// `self` 是否是 `other` 的前缀（含相等）。通配片段只和通配片段相等。
    #[must_use]
    pub fn is_prefix_of(&self, other: &Pointer) -> bool {
        self.segs.len() <= other.segs.len()
            && self.segs.iter().zip(&other.segs).all(|(a, b)| a == b)
    }
}

impl fmt::Display for Pointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[must_use]
pub fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn unescape(raw: &str) -> Result<String, PointerError> {
    if !raw.contains('~') {
        return Ok(raw.to_owned());
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '~' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => out.push('~'),
            Some('1') => out.push('/'),
            _ => return Err(PointerError::BadEscape),
        }
    }
    Ok(out)
}

/// RFC 6901 的数组下标：纯数字、无前导零。
fn index(key: &str) -> Option<usize> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse().ok()
}

fn collect<'a>(node: &'a Value, segs: &[Seg], out: &mut Vec<&'a Value>) {
    let Some((head, rest)) = segs.split_first() else {
        out.push(node);
        return;
    };
    match (head, node) {
        (Seg::Key(key), Value::Object(map)) => {
            if let Some(child) = map.get(key) {
                collect(child, rest, out);
            }
        }
        (Seg::Key(key), Value::Array(items)) => {
            if let Some(child) = index(key).and_then(|i| items.get(i)) {
                collect(child, rest, out);
            }
        }
        (Seg::Any, Value::Object(map)) => map.values().for_each(|child| collect(child, rest, out)),
        (Seg::Any, Value::Array(items)) => items.iter().for_each(|child| collect(child, rest, out)),
        _ => {}
    }
}

/// 命中的全部节点（只读）。
#[must_use]
pub fn find<'a>(root: &'a Value, ptr: &Pointer) -> Vec<&'a Value> {
    let mut out = Vec::new();
    collect(root, ptr.segs(), &mut out);
    out
}

/// 第一个命中的节点；无通配的指针至多命中一个。
#[must_use]
pub fn get<'a>(root: &'a Value, ptr: &Pointer) -> Option<&'a Value> {
    find(root, ptr).into_iter().next()
}

/// 对每个「叶片段的父节点」调用 `visit(parent, leaf)`。
/// `create`：途经缺失的对象键时补空对象；已有的非容器节点（字符串、数字…）不动，也不往里走。
fn walk_parents(
    node: &mut Value,
    segs: &[Seg],
    create: bool,
    visit: &mut dyn FnMut(&mut Value, &Seg),
) {
    let Some((head, rest)) = segs.split_first() else {
        return;
    };
    if rest.is_empty() {
        visit(node, head);
        return;
    }
    match (head, node) {
        (Seg::Key(key), Value::Object(map)) => {
            if create && !map.contains_key(key) {
                map.insert(key.clone(), Value::Object(Map::new()));
            }
            if let Some(child) = map.get_mut(key) {
                walk_parents(child, rest, create, visit);
            }
        }
        (Seg::Key(key), Value::Array(items)) => {
            if let Some(child) = index(key).and_then(|i| items.get_mut(i)) {
                walk_parents(child, rest, create, visit);
            }
        }
        (Seg::Any, Value::Object(map)) => map
            .values_mut()
            .for_each(|child| walk_parents(child, rest, create, visit)),
        (Seg::Any, Value::Array(items)) => items
            .iter_mut()
            .for_each(|child| walk_parents(child, rest, create, visit)),
        _ => {}
    }
}

/// 删除命中的节点，返回删掉的个数。叶片段是通配时不动（校验器也拒绝这种写法）。
pub fn remove(root: &mut Value, ptr: &Pointer) -> usize {
    let mut removed = 0;
    walk_parents(
        root,
        ptr.segs(),
        false,
        &mut |parent, leaf| match (leaf, parent) {
            (Seg::Key(key), Value::Object(map)) => {
                removed += usize::from(map.remove(key).is_some());
            }
            (Seg::Key(key), Value::Array(items)) => {
                if let Some(i) = index(key)
                    && i < items.len()
                {
                    items.remove(i);
                    removed += 1;
                }
            }
            _ => {}
        },
    );
    removed
}

/// 取走（删除并返回）第一个命中的节点；只用于无通配的指针。
pub fn take(root: &mut Value, ptr: &Pointer) -> Option<Value> {
    let mut taken = None;
    walk_parents(root, ptr.segs(), false, &mut |parent, leaf| {
        if taken.is_some() {
            return;
        }
        match (leaf, parent) {
            (Seg::Key(key), Value::Object(map)) => taken = map.remove(key),
            (Seg::Key(key), Value::Array(items)) => {
                if let Some(i) = index(key)
                    && i < items.len()
                {
                    taken = Some(items.remove(i));
                }
            }
            _ => {}
        }
    });
    taken
}

/// 写值，返回真正改变的个数。途经缺失的对象键会补空对象；数组元素只替换已有下标。
/// `if_absent`：叶子已存在（包括值为 `null`）就不动。
pub fn set(root: &mut Value, ptr: &Pointer, value: &Value, if_absent: bool) -> usize {
    let mut changed = 0;
    walk_parents(
        root,
        ptr.segs(),
        true,
        &mut |parent, leaf| match (leaf, parent) {
            (Seg::Key(key), Value::Object(map)) => match map.get(key) {
                Some(_) if if_absent => {}
                Some(old) if old == value => {}
                _ => {
                    map.insert(key.clone(), value.clone());
                    changed += 1;
                }
            },
            (Seg::Key(key), Value::Array(items)) => {
                if !if_absent
                    && let Some(slot) = index(key).and_then(|i| items.get_mut(i))
                    && *slot != *value
                {
                    *slot = value.clone();
                    changed += 1;
                }
            }
            _ => {}
        },
    );
    changed
}

/// 对每个已存在的命中节点调用 `f`（原地改）。
pub fn each_mut(root: &mut Value, ptr: &Pointer, f: &mut dyn FnMut(&mut Value)) {
    walk_parents(
        root,
        ptr.segs(),
        false,
        &mut |parent, leaf| match (leaf, parent) {
            (Seg::Key(key), Value::Object(map)) => {
                if let Some(node) = map.get_mut(key) {
                    f(node);
                }
            }
            (Seg::Key(key), Value::Array(items)) => {
                if let Some(node) = index(key).and_then(|i| items.get_mut(i)) {
                    f(node);
                }
            }
            (Seg::Any, Value::Object(map)) => map.values_mut().for_each(&mut *f),
            (Seg::Any, Value::Array(items)) => items.iter_mut().for_each(&mut *f),
            _ => {}
        },
    );
}

/// `keep_only` 的白名单树：列出的路径整棵保留，其余一律修剪。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeepTree {
    all: bool,
    children: BTreeMap<String, KeepTree>,
    any: Option<Box<KeepTree>>,
}

impl KeepTree {
    #[must_use]
    pub fn build<'a>(pointers: impl IntoIterator<Item = &'a Pointer>) -> Self {
        let mut tree = Self::default();
        for ptr in pointers {
            tree.insert(ptr.segs());
        }
        tree
    }

    fn insert(&mut self, segs: &[Seg]) {
        if self.all {
            return;
        }
        match segs.split_first() {
            None => {
                self.all = true;
                self.children.clear();
                self.any = None;
            }
            Some((Seg::Key(key), rest)) => {
                self.children.entry(key.clone()).or_default().insert(rest);
            }
            Some((Seg::Any, rest)) => self.any.get_or_insert_with(Box::default).insert(rest),
        }
    }

    /// 树里有没有覆盖 `target` 的整棵保留（`target` 本身或它的某个祖先被列出）。
    #[must_use]
    pub fn keeps_whole(&self, target: &[&str]) -> bool {
        if self.all {
            return true;
        }
        let Some((head, rest)) = target.split_first() else {
            return false;
        };
        self.children
            .get(*head)
            .is_some_and(|c| c.keeps_whole(rest))
            || self.any.as_deref().is_some_and(|c| c.keeps_whole(rest))
    }

    fn union(&self, other: &KeepTree) -> KeepTree {
        if self.all || other.all {
            return KeepTree {
                all: true,
                ..KeepTree::default()
            };
        }
        let mut children = self.children.clone();
        for (key, theirs) in &other.children {
            let merged = match children.get(key) {
                Some(mine) => mine.union(theirs),
                None => theirs.clone(),
            };
            children.insert(key.clone(), merged);
        }
        let any = match (&self.any, &other.any) {
            (Some(a), Some(b)) => Some(Box::new(a.union(b))),
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        };
        KeepTree {
            all: false,
            children,
            any,
        }
    }

    /// 修剪 `node`，返回删掉的对象成员 / 数组元素个数。
    pub fn prune(&self, node: &mut Value) -> usize {
        if self.all {
            return 0;
        }
        match node {
            Value::Object(map) => {
                let keys: Vec<String> = map.keys().cloned().collect();
                let mut removed = 0;
                for key in keys {
                    let keep = match (self.children.get(&key), self.any.as_deref()) {
                        (Some(a), Some(b)) => Some(a.union(b)),
                        (Some(a), None) => Some(a.clone()),
                        (None, Some(b)) => Some(b.clone()),
                        (None, None) => None,
                    };
                    match keep {
                        None => {
                            map.remove(&key);
                            removed += 1;
                        }
                        Some(sub) => {
                            if let Some(child) = map.get_mut(&key) {
                                removed += sub.prune(child);
                            }
                        }
                    }
                }
                removed
            }
            Value::Array(items) => {
                let old = std::mem::take(items);
                let mut removed = 0;
                for (i, mut item) in old.into_iter().enumerate() {
                    let keep = match (self.children.get(&i.to_string()), self.any.as_deref()) {
                        (Some(a), Some(b)) => Some(a.union(b)),
                        (Some(a), None) => Some(a.clone()),
                        (None, Some(b)) => Some(b.clone()),
                        (None, None) => None,
                    };
                    match keep {
                        None => removed += 1,
                        Some(sub) => {
                            removed += sub.prune(&mut item);
                            items.push(item);
                        }
                    }
                }
                removed
            }
            _ => 0,
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
    fn parse_accepts_rfc_forms_and_wildcards() {
        assert_eq!(p("/a/b").segs().len(), 2);
        assert_eq!(p("/a~1b").segs(), [Seg::Key("a/b".into())]);
        assert_eq!(p("/a~0b").segs(), [Seg::Key("a~b".into())]);
        assert_eq!(p("/m/*/role").segs()[1], Seg::Any);
        assert!(p("/m/*/role").has_wildcard());
        assert!(p("/m/*").leaf_is_wildcard());
        assert!(!p("/m/*/role").leaf_is_wildcard());
    }

    #[test]
    fn parse_rejects_malformed_pointers() {
        assert_eq!(Pointer::parse(""), Err(PointerError::Root));
        assert_eq!(Pointer::parse("a"), Err(PointerError::NoLeadingSlash));
        assert_eq!(Pointer::parse("/"), Err(PointerError::EmptySegment));
        assert_eq!(Pointer::parse("/a//b"), Err(PointerError::EmptySegment));
        assert_eq!(Pointer::parse("/a~2"), Err(PointerError::BadEscape));
        assert_eq!(Pointer::parse("/a~"), Err(PointerError::BadEscape));
        let deep = "/a".repeat(MAX_DEPTH + 1);
        assert_eq!(Pointer::parse(&deep), Err(PointerError::TooDeep));
        assert!(Pointer::parse(&"/a".repeat(MAX_DEPTH)).is_ok());
        let long_segment = format!("/{}", "x".repeat(MAX_SEGMENT_LEN + 1));
        assert_eq!(
            Pointer::parse(&long_segment),
            Err(PointerError::SegmentTooLong)
        );
        let long = format!("/{}", "x/".repeat(MAX_POINTER_LEN));
        assert_eq!(Pointer::parse(&long), Err(PointerError::TooLong));
    }

    #[test]
    fn escape_round_trips_awkward_keys() {
        let text = Pointer::from_keys(["a/b", "c~d"]);
        assert_eq!(text, "/a~1b/c~0d");
        assert_eq!(
            p(&text).segs(),
            [Seg::Key("a/b".into()), Seg::Key("c~d".into())]
        );
    }

    #[test]
    fn find_walks_objects_arrays_and_wildcards() {
        let doc = json!({"m": [{"role": "a"}, {"role": "b"}, {"x": 1}], "k": {"role": "c"}});
        assert_eq!(find(&doc, &p("/m/*/role")), vec![&json!("a"), &json!("b")]);
        assert_eq!(get(&doc, &p("/m/1/role")), Some(&json!("b")));
        assert_eq!(get(&doc, &p("/m/01/role")), None, "前导零不是下标");
        assert_eq!(get(&doc, &p("/m/-/role")), None);
        assert_eq!(find(&doc, &p("/k/*")).len(), 1);
        assert_eq!(get(&doc, &p("/nope")), None);
    }

    #[test]
    fn remove_deletes_matches_and_reports_the_count() {
        let mut doc = json!({"a": 1, "m": [{"r": 1, "k": 2}, {"r": 3}], "s": "x"});
        assert_eq!(remove(&mut doc, &p("/a")), 1);
        assert_eq!(remove(&mut doc, &p("/a")), 0);
        assert_eq!(remove(&mut doc, &p("/m/*/r")), 2);
        assert_eq!(doc, json!({"m": [{"k": 2}, {}], "s": "x"}));
        assert_eq!(remove(&mut doc, &p("/s/deeper")), 0, "标量里没有东西可删");
        assert_eq!(remove(&mut doc, &p("/m/*")), 0, "叶子通配不删");
        assert_eq!(remove(&mut doc, &p("/m/0")), 1);
        assert_eq!(doc, json!({"m": [{}], "s": "x"}));
    }

    #[test]
    fn set_creates_objects_but_never_array_elements() {
        let mut doc = json!({"a": 1, "arr": [1, 2]});
        assert_eq!(
            set(&mut doc, &p("/thinking/type"), &json!("enabled"), false),
            1
        );
        assert_eq!(doc["thinking"], json!({"type": "enabled"}));
        assert_eq!(
            set(&mut doc, &p("/thinking/type"), &json!("enabled"), false),
            0,
            "同值不算改"
        );
        assert_eq!(set(&mut doc, &p("/arr/1"), &json!(9), false), 1);
        assert_eq!(set(&mut doc, &p("/arr/5"), &json!(9), false), 0, "不追加");
        assert_eq!(
            set(&mut doc, &p("/a/deep"), &json!(1), false),
            0,
            "标量不当容器"
        );
        assert_eq!(doc["a"], json!(1));
    }

    #[test]
    fn set_if_absent_keeps_existing_values_including_null() {
        let mut doc = json!({"a": null, "b": 1});
        assert_eq!(set(&mut doc, &p("/a"), &json!(5), true), 0);
        assert_eq!(set(&mut doc, &p("/b"), &json!(5), true), 0);
        assert_eq!(set(&mut doc, &p("/c"), &json!(5), true), 1);
        assert_eq!(doc, json!({"a": null, "b": 1, "c": 5}));
    }

    #[test]
    fn set_with_wildcard_touches_only_existing_parents() {
        let mut doc = json!({"m": [{"role": "a"}, {"role": "b"}], "e": []});
        assert_eq!(set(&mut doc, &p("/m/*/role"), &json!("x"), false), 2);
        assert_eq!(set(&mut doc, &p("/e/*/role"), &json!("x"), false), 0);
        assert_eq!(doc, json!({"m": [{"role": "x"}, {"role": "x"}], "e": []}));
    }

    #[test]
    fn take_removes_and_returns() {
        let mut doc = json!({"a": {"b": 1}});
        assert_eq!(take(&mut doc, &p("/a/b")), Some(json!(1)));
        assert_eq!(take(&mut doc, &p("/a/b")), None);
        assert_eq!(doc, json!({"a": {}}));
    }

    #[test]
    fn keep_tree_prunes_to_the_listed_paths() {
        let mut doc = json!({
            "model": "m", "messages": [{"role": "u", "content": "c", "name": "n"}],
            "tools": [1], "temperature": 1
        });
        let ptrs = [
            p("/model"),
            p("/messages/*/role"),
            p("/messages/*/content"),
            p("/temperature"),
        ];
        let tree = KeepTree::build(&ptrs);
        let removed = tree.prune(&mut doc);
        assert_eq!(removed, 2, "tools 与 name");
        assert_eq!(
            doc,
            json!({"model": "m", "messages": [{"role": "u", "content": "c"}], "temperature": 1})
        );
        assert_eq!(tree.prune(&mut doc), 0, "幂等");
    }

    #[test]
    fn keep_tree_listing_a_node_keeps_its_whole_subtree() {
        let mut doc = json!({"a": {"x": 1, "y": 2}, "b": 3});
        let tree = KeepTree::build(&[p("/a")]);
        assert_eq!(tree.prune(&mut doc), 1);
        assert_eq!(doc, json!({"a": {"x": 1, "y": 2}}));
        assert!(tree.keeps_whole(&["a"]));
        assert!(tree.keeps_whole(&["a", "x"]));
        assert!(!tree.keeps_whole(&["b"]));
    }

    #[test]
    fn keep_tree_unions_explicit_and_wildcard_branches() {
        let mut doc = json!({"m": [{"role": "u", "content": "c", "name": "n"}]});
        let tree = KeepTree::build(&[p("/m/0/role"), p("/m/*/content")]);
        tree.prune(&mut doc);
        assert_eq!(doc, json!({"m": [{"role": "u", "content": "c"}]}));
    }
}
