//! 规则集的校验与编译。写入（控制台）、加载（网关）、内置数据（CI）走同一个入口，
//! 所以「线上能跑的规则」和「写入时放行的规则」不会漂移。
//!
//! 校验分两类结论：
//! - **硬错误**（[`Issue`]）：结构、限额、受保护路径、计费关键路径缺确认、阶段与操作不匹配。任何场景都不放行。
//! - **授权信息**（[`Validated::sensitive`]）：写入了敏感路径（或响应里的计费关键路径）的操作签名。是否放行取决于调用方有
//!   没有 `compat.sensitive`，引擎不懂权限，只负责报出来。
//!
//! 写了却静默不生效的规则是最坏的体验，所以凡是网关当前不会执行的组合（比如非 OpenAI chat 方言的响应规则、
//! 能力规则里的 `stream` 选择器）一律在这里拒绝并说明原因。

use crate::decimal::Decimal;
use crate::exec::{Bound, CompiledOp, UsageSlot};
use crate::expr::{Expr, Slot};
use crate::pointer::{KeepTree, Pointer};
use crate::protect::{self, Access, PathClass};
use crate::rule::{CAPABILITY_FACTS, ErrorWhen, OP_NAMES, Op, Phase, Rule, Selector};
use crate::select::{CompiledSelector, CompiledWhen, DIALECTS, OPERATIONS};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fmt;

pub const MAX_RULES: usize = 256;
pub const MAX_OPS: usize = 32;
pub const MAX_PATHS_PER_OP: usize = 256;
pub const MAX_TABLE_ENTRIES: usize = 256;
pub const MAX_DOC_BYTES: usize = 256 * 1024;
pub const MAX_ID_LEN: usize = 64;
pub const MAX_SELECTOR_ITEMS: usize = 64;
pub const MAX_PATTERN_LEN: usize = 128;
pub const MAX_ISSUES: usize = 50;
/// `each` 的嵌套层数。
pub const MAX_EACH_DEPTH: usize = 2;
/// `when` 每个字段的条目数。
pub const MAX_WHEN_ITEMS: usize = 16;
pub const MAX_RETRY: u8 = 3;
pub const MAX_COOLDOWN_SECS: u32 = 86_400;

/// 响应规则目前只在这个方言的上游上执行（网关只在 `openai_chat` 的调用上接了响应阶段）。
const RESPONSE_DIALECT: &str = "openai.chat";

/// 错误规则只许匹配网关**不替它们决定去向**的那类 HTTP 状态：401 / 402 / 403 / 408 / 429 / 5xx
/// 在网关里有成套的重试、冷却与 key 健康语义，规则不能改写。
#[must_use]
pub fn error_status_is_reactable(status: u16) -> bool {
    (400..=499).contains(&status) && !matches!(status, 401 | 402 | 403 | 408 | 429)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    pub detail: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)?;
        if let Some(rule) = &self.rule {
            write!(f, " [{rule}]")?;
        }
        write!(f, ": {}", self.detail)
    }
}

/// 校验并编译过的规则；字段私有，外面只能通过 [`Validated`] 拿到。
#[derive(Debug, Clone)]
pub struct ValidRule {
    pub(crate) rule: Rule,
    pub(crate) selector: CompiledSelector,
    pub(crate) when: Option<CompiledWhen>,
    pub(crate) ops: Vec<CompiledOp>,
}

impl ValidRule {
    #[must_use]
    pub fn rule(&self) -> &Rule {
        &self.rule
    }
}

/// 一个通过校验的规则集。
#[derive(Debug, Clone, Default)]
pub struct Validated {
    pub(crate) rules: Vec<ValidRule>,
    sensitive: Vec<String>,
}

impl Validated {
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().map(|r| &r.rule)
    }

    /// 写入敏感路径的操作签名（`规则 id|操作 JSON`）。调用方拿新旧两版的差集：
    /// 差集非空才需要 `compat.sensitive`，只是原样保存旧规则不需要。
    #[must_use]
    pub fn sensitive(&self) -> &[String] {
        &self.sensitive
    }

    /// 回到文档形态（API 回显、版本落库）。
    #[must_use]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self.rules().collect::<Vec<_>>()).unwrap_or(Value::Array(Vec::new()))
    }
}

fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

struct Checker<'a> {
    rule_id: &'a str,
    index: usize,
    ack: bool,
    issues: Vec<Issue>,
    sensitive: Vec<String>,
    /// 正在校验的阶段：选路径注册表，也限定可用的操作。`retry_same.mutate` 里临时切成 `request`。
    phase: Phase,
    /// `each` 子操作所在位置的绝对前缀（只用来给路径分级）；顶层为 `None`。
    prefix: Option<Pointer>,
    each_depth: usize,
    /// 整条规则里已校验的操作数（含子操作），封顶 [`MAX_OPS`]。
    op_count: usize,
    too_many_ops_reported: bool,
}

impl Checker<'_> {
    fn issue(&mut self, code: &'static str, detail: impl Into<String>) {
        self.issues.push(Issue {
            code,
            rule: Some(self.rule_id.to_owned()),
            index: Some(self.index),
            detail: detail.into(),
        });
    }

    fn pointer(&mut self, text: &str, what: &str) -> Option<Pointer> {
        match Pointer::parse(text) {
            Ok(ptr) => Some(ptr),
            Err(error) => {
                self.issue("invalid_path", format!("{what} `{text}`: {error}"));
                None
            }
        }
    }

    /// 分级用的绝对路径：`each` 里的子操作路径相对元素，拼上元素的前缀才是文档里的真实位置。
    fn absolute(&mut self, ptr: &Pointer) -> Option<Pointer> {
        let Some(prefix) = &self.prefix else {
            return Some(ptr.clone());
        };
        match prefix.join(ptr) {
            Ok(abs) => Some(abs),
            Err(error) => {
                self.issue(
                    "invalid_path",
                    format!("each 里的路径 `{ptr}` 拼上外层后: {error}"),
                );
                None
            }
        }
    }

    /// 按注册表给一次访问定性：受保护直接拒绝；计费关键要有确认（响应里还要 `compat.sensitive`）；
    /// 敏感写入记下签名。
    fn access(&mut self, ptr: &Pointer, access: Access, billing_safe: bool, signature: &str) {
        let Some(abs) = self.absolute(ptr) else {
            return;
        };
        match protect::classify_in(self.phase, &abs, access) {
            PathClass::Protected => self.issue(
                "protected_path",
                format!("`{abs}` 是受保护路径（主链 / 路由指令 / 对话本体），规则不可写"),
            ),
            PathClass::BillingCritical => {
                if self.phase == Phase::Response {
                    // 动用量事实就是动账：除了确认，写入者还要有 compat.sensitive
                    self.sensitive.push(signature.to_owned());
                }
                if !billing_safe && !self.ack {
                    let why = if self.phase == Phase::Response {
                        "影响用量 / 计价事实"
                    } else {
                        "影响流式用量计量"
                    };
                    self.issue(
                        "billing_critical_ack_required",
                        format!("`{abs}` {why}，需在规则上设 billing_critical_ack: true"),
                    );
                }
            }
            PathClass::Sensitive => self.sensitive.push(signature.to_owned()),
            PathClass::Free => {}
        }
    }

    fn paths(&mut self, texts: &[String], signature: &str, access: Access) -> Vec<Pointer> {
        if texts.is_empty() {
            self.issue("no_paths", "paths 不能为空");
            return Vec::new();
        }
        if texts.len() > MAX_PATHS_PER_OP {
            self.issue(
                "too_many_paths",
                format!("单个操作最多 {MAX_PATHS_PER_OP} 条路径"),
            );
            return Vec::new();
        }
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            let Some(ptr) = self.pointer(text, "path") else {
                continue;
            };
            if ptr.leaf_is_wildcard() {
                self.issue(
                    "wildcard_leaf",
                    format!("`{text}` 以通配结尾；请指到具体字段"),
                );
                continue;
            }
            self.access(&ptr, access, false, signature);
            out.push(ptr);
        }
        out
    }

    fn move_pair(
        &mut self,
        from: &str,
        to: &str,
        signature: &str,
        removes_source: bool,
    ) -> Option<(Pointer, Pointer)> {
        let from = self.pointer(from, "from");
        let to = self.pointer(to, "to");
        let (from, to) = (from?, to?);
        if from.has_wildcard() || to.has_wildcard() {
            self.issue(
                "wildcard_not_allowed",
                "rename / copy 的路径不支持通配；成批处理请用 each",
            );
            return None;
        }
        if from.is_prefix_of(&to) || to.is_prefix_of(&from) {
            self.issue("overlapping_paths", format!("`{from}` 与 `{to}` 互为前缀"));
            return None;
        }
        if removes_source {
            self.access(&from, Access::Remove, false, signature);
        }
        self.access(&to, Access::Write, false, signature);
        Some((from, to))
    }

    fn bound(&mut self, number: &serde_json::Number, which: &str) -> Option<Bound> {
        let Some(decimal) = Decimal::parse(&number.to_string()) else {
            self.issue(
                "invalid_number",
                format!("{which} `{number}` 不是可比较的十进制数"),
            );
            return None;
        };
        Some(Bound {
            number: number.clone(),
            decimal,
        })
    }

    /// 校验一组子操作（`each` 的 `do`、`retry_same` 的 `mutate`）；任何一个不过就整组作废。
    fn nested(&mut self, ops: &[Op], signature: &str) -> Option<Vec<CompiledOp>> {
        let mut compiled = Vec::with_capacity(ops.len());
        let mut ok = true;
        for op in ops {
            match self.op(op, signature) {
                Some(c) => compiled.push(c),
                None => ok = false,
            }
        }
        ok.then_some(compiled)
    }

    // 每种操作一个分支，各自的校验互不相干；拆成函数只会把一张操作表打散
    #[allow(clippy::too_many_lines)]
    fn op(&mut self, op: &Op, signature: &str) -> Option<CompiledOp> {
        self.op_count += 1;
        if self.op_count > MAX_OPS {
            if !self.too_many_ops_reported {
                self.too_many_ops_reported = true;
                self.issue(
                    "too_many_ops",
                    format!("单条规则最多 {MAX_OPS} 个操作（含 each / mutate 里的子操作）"),
                );
            }
            return None;
        }
        if !op.allowed_in(self.phase) {
            self.issue(
                "op_not_allowed_in_phase",
                format!("操作 `{}` 不能用在 {} 阶段", op.name(), self.phase.as_str()),
            );
            return None;
        }
        if self.prefix.is_some()
            && matches!(
                op,
                Op::KeepOnly { .. } | Op::RoleMap { .. } | Op::UsageMap { .. }
            )
        {
            self.issue(
                "op_not_allowed_in_each",
                format!("操作 `{}` 不能放进 each", op.name()),
            );
            return None;
        }
        match op {
            Op::Remove { paths } => Some(CompiledOp::Remove(self.paths(
                paths,
                signature,
                Access::Remove,
            ))),
            Op::KeepOnly { paths } => {
                if paths.is_empty() {
                    self.issue("no_paths", "paths 不能为空");
                    return None;
                }
                if paths.len() > MAX_PATHS_PER_OP {
                    self.issue(
                        "too_many_paths",
                        format!("单个操作最多 {MAX_PATHS_PER_OP} 条路径"),
                    );
                    return None;
                }
                let mut listed: Vec<Pointer> = paths
                    .iter()
                    .filter_map(|text| self.pointer(text, "path"))
                    .collect();
                if listed.len() != paths.len() {
                    return None;
                }
                listed.extend(protect::implicit_keep());
                let tree = KeepTree::build(&listed);
                if protect::billing_critical_dropped(&tree) && !self.ack {
                    self.issue(
                        "billing_critical_ack_required",
                        "白名单没有保留 /stream_options，会丢掉流式用量，需设 billing_critical_ack: true",
                    );
                }
                Some(CompiledOp::KeepOnly(tree))
            }
            Op::Set { path, value, mode } => {
                let ptr = self.pointer(path, "path")?;
                if ptr.leaf_is_wildcard() {
                    self.issue(
                        "wildcard_leaf",
                        format!("`{path}` 以通配结尾；请指到具体字段"),
                    );
                    return None;
                }
                self.access(
                    &ptr,
                    Access::Write,
                    self.phase == Phase::Request
                        && self.prefix.is_none()
                        && protect::billing_safe_set(&ptr, value),
                    signature,
                );
                Some(CompiledOp::Set {
                    path: ptr,
                    value: value.clone(),
                    if_absent: mode.if_absent(),
                })
            }
            Op::Rename { from, to, mode } => {
                let (from, to) = self.move_pair(from, to, signature, true)?;
                Some(CompiledOp::Rename {
                    from,
                    to,
                    if_absent: mode.if_absent(),
                })
            }
            Op::Copy { from, to, mode } => {
                let (from, to) = self.move_pair(from, to, signature, false)?;
                Some(CompiledOp::Copy {
                    from,
                    to,
                    if_absent: mode.if_absent(),
                })
            }
            Op::Clamp { path, min, max } => {
                let ptr = self.pointer(path, "path")?;
                if min.is_none() && max.is_none() {
                    self.issue("clamp_bounds", "clamp 至少要有 min 或 max");
                    return None;
                }
                let min = match min {
                    Some(n) => Some(self.bound(n, "min")?),
                    None => None,
                };
                let max = match max {
                    Some(n) => Some(self.bound(n, "max")?),
                    None => None,
                };
                if let (Some(lo), Some(hi)) = (&min, &max)
                    && lo.decimal > hi.decimal
                {
                    self.issue("clamp_bounds", "min 大于 max");
                    return None;
                }
                self.access(&ptr, Access::Write, false, signature);
                Some(CompiledOp::Clamp {
                    path: ptr,
                    min,
                    max,
                })
            }
            Op::MapValue { path, table } => {
                let ptr = self.pointer(path, "path")?;
                if table.is_empty() || table.len() > MAX_TABLE_ENTRIES {
                    self.issue(
                        "invalid_table",
                        format!("table 需有 1 到 {MAX_TABLE_ENTRIES} 项"),
                    );
                    return None;
                }
                self.access(&ptr, Access::Write, false, signature);
                let table: BTreeMap<String, Value> =
                    table.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                Some(CompiledOp::MapValue { path: ptr, table })
            }
            Op::RoleMap { from, to } => {
                if from.is_empty()
                    || to.is_empty()
                    || from == to
                    || from.len() > 64
                    || to.len() > 64
                {
                    self.issue(
                        "invalid_role",
                        "role_map 的 from / to 需非空、不同、不超过 64 字符",
                    );
                    return None;
                }
                let mut targets = Vec::new();
                for text in ["/messages/*/role", "/input/*/role"] {
                    let ptr = Pointer::parse(text).ok()?;
                    self.access(&ptr, Access::Write, false, signature);
                    targets.push(ptr);
                }
                Some(CompiledOp::RoleMap {
                    targets,
                    from: from.clone(),
                    to: to.clone(),
                })
            }
            Op::Each { path, ops } => self.each(path, ops, signature),
            Op::UsageMap { slots } => self.usage_map(slots, signature),
            Op::Capability { facts } => {
                if facts.is_empty() {
                    self.issue("no_facts", "facts 不能为空");
                    return None;
                }
                let unknown: Vec<&str> = facts
                    .keys()
                    .map(String::as_str)
                    .filter(|name| !CAPABILITY_FACTS.contains(name))
                    .collect();
                if !unknown.is_empty() {
                    self.issue(
                        "unknown_fact",
                        format!(
                            "未知的能力事实 {}；可用：{}",
                            unknown.join(", "),
                            CAPABILITY_FACTS.join(", ")
                        ),
                    );
                    return None;
                }
                Some(CompiledOp::Capability(facts.clone()))
            }
            Op::RetrySame { mutate, max } => {
                let max = max.unwrap_or(1);
                if !(1..=MAX_RETRY).contains(&max) {
                    self.issue("invalid_retry", format!("max 需在 1 到 {MAX_RETRY} 之间"));
                    return None;
                }
                if mutate.is_empty() {
                    self.issue(
                        "no_ops",
                        "retry_same 的 mutate 不能为空：原样重发同一个请求只会得到同一个错误",
                    );
                    return None;
                }
                // mutate 是改发往上游的请求体：按请求阶段校验（受保护路径、计费关键、敏感都照请求那张表）
                let saved = (
                    std::mem::replace(&mut self.phase, Phase::Request),
                    self.prefix.take(),
                    std::mem::replace(&mut self.each_depth, 0),
                );
                let compiled = self.nested(mutate, signature);
                (self.phase, self.prefix, self.each_depth) = saved;
                Some(CompiledOp::RetrySame {
                    mutate: compiled?,
                    max,
                })
            }
            Op::Failover { cooldown_secs } => {
                if cooldown_secs.is_some_and(|secs| secs == 0 || secs > MAX_COOLDOWN_SECS) {
                    self.issue(
                        "invalid_cooldown",
                        format!("cooldown_secs 需在 1 到 {MAX_COOLDOWN_SECS} 之间"),
                    );
                    return None;
                }
                Some(CompiledOp::Failover {
                    cooldown_secs: *cooldown_secs,
                })
            }
            Op::Fail { status, code } => {
                if status.is_none() && code.is_none() {
                    self.issue("no_fields", "fail 至少要改写 status / code 之一");
                    return None;
                }
                if status.is_some_and(|s| !(400..=599).contains(&s)) {
                    self.issue("invalid_status", "fail 的 status 需在 400 到 599 之间");
                    return None;
                }
                if let Some(code) = code
                    && (code.is_empty()
                        || code.len() > MAX_ID_LEN
                        || !code.chars().all(|c| {
                            c.is_ascii_lowercase()
                                || c.is_ascii_digit()
                                || matches!(c, '_' | '.' | '-')
                        }))
                {
                    self.issue(
                        "invalid_code",
                        format!("code 需为小写字母 / 数字 / . _ -，不超过 {MAX_ID_LEN} 字符"),
                    );
                    return None;
                }
                Some(CompiledOp::Fail {
                    status: *status,
                    code: code.clone(),
                })
            }
        }
    }

    fn each(&mut self, path: &str, ops: &[Op], signature: &str) -> Option<CompiledOp> {
        if self.each_depth >= MAX_EACH_DEPTH {
            self.issue(
                "each_too_deep",
                format!("each 最多嵌套 {MAX_EACH_DEPTH} 层"),
            );
            return None;
        }
        let container = self.pointer(path, "path")?;
        if container.leaf_is_wildcard() {
            self.issue(
                "wildcard_leaf",
                format!("`{path}` 以通配结尾；each 的 path 指向数组（或对象）本身"),
            );
            return None;
        }
        if ops.is_empty() {
            self.issue("no_ops", "each 的 do 不能为空");
            return None;
        }
        let items = match container.children() {
            Ok(items) => items,
            Err(error) => {
                self.issue("invalid_path", format!("path `{path}`: {error}"));
                return None;
            }
        };
        let inner_prefix = self.absolute(&items)?;
        let saved = (self.prefix.replace(inner_prefix), self.each_depth);
        self.each_depth += 1;
        let compiled = self.nested(ops, signature);
        (self.prefix, self.each_depth) = saved;
        Some(CompiledOp::Each {
            items,
            ops: compiled?,
        })
    }

    fn usage_map(
        &mut self,
        slots: &BTreeMap<String, String>,
        signature: &str,
    ) -> Option<CompiledOp> {
        if slots.is_empty() {
            self.issue("no_slots", "slots 不能为空");
            return None;
        }
        let mut compiled = Vec::with_capacity(slots.len());
        let mut ok = true;
        for (name, text) in slots {
            let Some(slot) = Slot::parse(name) else {
                let names: Vec<&str> = Slot::ALL.iter().map(|s| s.name()).collect();
                self.issue(
                    "unknown_slot",
                    format!("未知的用量槽位 `{name}`；可用：{}", names.join(", ")),
                );
                ok = false;
                continue;
            };
            match Expr::parse(text) {
                Ok(expr) => {
                    let Ok(target) = Pointer::parse(slot.pointer()) else {
                        ok = false;
                        continue;
                    };
                    compiled.push(UsageSlot { target, expr });
                }
                Err(error) => {
                    self.issue(
                        "invalid_expr",
                        format!("槽位 {name} 的表达式 `{text}`: {error}"),
                    );
                    ok = false;
                }
            }
        }
        // 写的是用量事实：恒为计费关键（要 ack，且写入者需要 compat.sensitive）
        let usage = Pointer::parse("/usage").ok()?;
        self.access(&usage, Access::Write, false, signature);
        ok.then_some(CompiledOp::UsageMap(compiled))
    }

    fn selector(&mut self, select: &Selector) {
        let lists: [(&str, &Vec<String>); 7] = [
            ("vendor", &select.vendor),
            ("host", &select.host),
            ("model", &select.model),
            ("client_model", &select.client_model),
            ("dialect", &select.dialect),
            ("operation", &select.operation),
            ("client", &select.client),
        ];
        for (name, items) in lists {
            if items.len() > MAX_SELECTOR_ITEMS {
                self.issue(
                    "selector_too_long",
                    format!("select.{name} 最多 {MAX_SELECTOR_ITEMS} 项"),
                );
                continue;
            }
            for item in items {
                if item.is_empty()
                    || item.len() > MAX_PATTERN_LEN
                    || item.chars().any(char::is_whitespace)
                {
                    self.issue(
                        "selector_invalid_item",
                        format!("select.{name} 里有空项 / 含空白 / 超过 {MAX_PATTERN_LEN} 字符：`{item}`"),
                    );
                }
            }
        }
        for item in &select.dialect {
            if !DIALECTS.contains(&item.as_str()) {
                self.issue(
                    "selector_unknown_value",
                    format!(
                        "select.dialect 不认识 `{item}`，可用：{}",
                        DIALECTS.join(", ")
                    ),
                );
            }
        }
        for item in &select.operation {
            if !OPERATIONS.contains(&item.as_str()) {
                self.issue(
                    "selector_unknown_value",
                    format!(
                        "select.operation 不认识 `{item}`，可用：{}",
                        OPERATIONS.join(", ")
                    ),
                );
            }
        }
        for item in &select.vendor {
            let ok = item
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
            if !ok {
                self.issue(
                    "selector_invalid_item",
                    format!("select.vendor `{item}` 含非法字符"),
                );
            }
        }
    }

    /// 选择器在各阶段的额外限制：只写网关真会去匹配的字段。
    fn selector_for_phase(&mut self, select: &Selector) {
        match self.phase {
            Phase::Capability => {
                // 能力是厂商 / 模型的事实，在渠道候选加载时合并：此时还没有请求，也就没有这些维度
                let unsupported: Vec<&str> = [
                    ("client_model", !select.client_model.is_empty()),
                    ("dialect", !select.dialect.is_empty()),
                    ("operation", !select.operation.is_empty()),
                    ("stream", select.stream.is_some()),
                    ("client", !select.client.is_empty()),
                ]
                .into_iter()
                .filter_map(|(name, used)| used.then_some(name))
                .collect();
                if !unsupported.is_empty() {
                    self.issue(
                        "selector_not_supported_in_phase",
                        format!(
                            "能力规则的 select 只能用 vendor / host / model，不支持 {}",
                            unsupported.join(", ")
                        ),
                    );
                }
            }
            Phase::Response
                if select.dialect.is_empty()
                    || select.dialect.iter().any(|d| d != RESPONSE_DIALECT) =>
            {
                self.issue(
                    "response_dialect_unsupported",
                    format!(
                        "响应规则目前只在 {RESPONSE_DIALECT} 方言的上游上执行，需写 select.dialect: [\"{RESPONSE_DIALECT}\"]"
                    ),
                );
            }
            _ => {}
        }
    }

    /// 规则本身（不是操作）在各阶段的约束：选择器范围、`required`、`when`、错误规则恰好一个动作。
    fn rule_shape(&mut self, rule: &Rule) -> Option<CompiledWhen> {
        self.selector_for_phase(&rule.select);
        if rule.required && rule.phase != Phase::Request {
            self.issue(
                "required_not_supported",
                format!(
                    "required 只用于 request 阶段（{} 阶段没有失败关闭的语义）",
                    rule.phase.as_str()
                ),
            );
        }
        let when = match (&rule.when, rule.phase) {
            (Some(w), Phase::Error) => self.when(w),
            (None, Phase::Error) => {
                self.issue(
                    "when_required",
                    "error 规则必须写 when（什么样的上游错误才触发）",
                );
                None
            }
            (Some(_), _) => {
                self.issue("when_not_allowed", "when 只用于 error 阶段");
                None
            }
            (None, _) => None,
        };
        if rule.phase == Phase::Error && rule.ops.len() > 1 {
            self.issue(
                "error_rule_single_action",
                "error 规则的 do 里恰好一个动作（retry_same / failover / fail）",
            );
        }
        when
    }

    fn when(&mut self, when: &ErrorWhen) -> Option<CompiledWhen> {
        if when.is_empty() {
            self.issue(
                "when_empty",
                "error 规则的 when 至少要有 status / code / body_contains 之一",
            );
            return None;
        }
        let mut ok = true;
        for (name, len) in [
            ("status", when.status.len()),
            ("code", when.code.len()),
            ("body_contains", when.body_contains.len()),
        ] {
            if len > MAX_WHEN_ITEMS {
                self.issue(
                    "when_too_long",
                    format!("when.{name} 最多 {MAX_WHEN_ITEMS} 项"),
                );
                ok = false;
            }
        }
        for status in &when.status {
            if !error_status_is_reactable(*status) {
                self.issue(
                    "error_status_not_reactable",
                    format!(
                        "状态码 {status} 由网关的重试 / 冷却矩阵处理，规则不能改写；可用 400–499，不含 401 402 403 408 429"
                    ),
                );
                ok = false;
            }
        }
        for (name, items) in [("code", &when.code), ("body_contains", &when.body_contains)] {
            for item in items {
                if item.trim().is_empty()
                    || item.len() > MAX_PATTERN_LEN
                    || item.chars().any(char::is_control)
                {
                    self.issue(
                        "when_invalid_item",
                        format!("when.{name} 里有空项 / 含控制字符 / 超过 {MAX_PATTERN_LEN} 字符"),
                    );
                    ok = false;
                }
            }
        }
        ok.then(|| CompiledWhen::compile(when))
    }
}

/// 递归检查 `do` / `mutate` 里的操作名：给出「可用操作」比一句 serde 的 unknown variant 好懂。
fn precheck_ops(
    ops: &[Value],
    at: &str,
    early: &dyn Fn(&'static str, String) -> Issue,
    out: &mut Vec<Issue>,
) {
    for (i, op) in ops.iter().enumerate() {
        match op.get("op").and_then(Value::as_str) {
            None => out.push(early("missing_op", format!("{at}[{i}] 缺少 op"))),
            Some(name) if !OP_NAMES.contains(&name) => out.push(early(
                "unknown_op",
                format!(
                    "{at}[{i}]: 未知操作 `{name}`，可用：{}",
                    OP_NAMES.join(", ")
                ),
            )),
            Some("each") => {
                if let Some(inner) = op.get("do").and_then(Value::as_array) {
                    precheck_ops(inner, &format!("{at}[{i}].do"), early, out);
                }
            }
            Some("retry_same") => {
                if let Some(inner) = op.get("mutate").and_then(Value::as_array) {
                    precheck_ops(inner, &format!("{at}[{i}].mutate"), early, out);
                }
            }
            Some(_) => {}
        }
    }
}

fn validate_rule(index: usize, item: &Value) -> Result<(ValidRule, Vec<String>), Vec<Issue>> {
    let hint = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    let early = |code: &'static str, detail: String| Issue {
        code,
        rule: Some(hint.clone()),
        index: Some(index),
        detail,
    };
    let mut pre = Vec::new();
    if let Some(ops) = item.get("do").and_then(Value::as_array) {
        precheck_ops(ops, "do", &early, &mut pre);
    }
    if !pre.is_empty() {
        return Err(pre);
    }
    let rule: Rule = serde_json::from_value(item.clone())
        .map_err(|error| vec![early("invalid_rule", error.to_string())])?;
    let mut checker = Checker {
        rule_id: &rule.id,
        index,
        ack: rule.billing_critical_ack,
        issues: Vec::new(),
        sensitive: Vec::new(),
        phase: rule.phase,
        prefix: None,
        each_depth: 0,
        op_count: 0,
        too_many_ops_reported: false,
    };
    if !is_valid_id(&rule.id) {
        checker.issue(
            "invalid_id",
            format!("id 需为小写字母 / 数字 / . _ -，以字母或数字开头，不超过 {MAX_ID_LEN} 字符"),
        );
    }
    if !rule.phase.enabled() {
        checker.issue(
            "phase_not_enabled",
            format!(
                "阶段 {} 还没有启用，当前可用：request / response / error / capability",
                rule.phase.as_str()
            ),
        );
    }
    if rule.cond.is_some() {
        checker.issue("cel_not_enabled", "条件表达式（if）还没有启用；字段已预留");
    }
    // 只有 disabled 的覆盖项（墓碑）可以没有操作，也不受阶段规则约束
    let tombstone = rule.disabled && rule.ops.is_empty();
    if rule.ops.is_empty() && !rule.disabled {
        checker.issue(
            "no_ops",
            "do 不能为空（只有 disabled 的覆盖项可以没有操作）",
        );
    }
    if rule.ops.len() > MAX_OPS {
        checker.issue("too_many_ops", format!("单条规则最多 {MAX_OPS} 个操作"));
        checker.too_many_ops_reported = true;
    }
    checker.selector(&rule.select);
    let when = if rule.phase.enabled() && !tombstone {
        checker.rule_shape(&rule)
    } else {
        None
    };
    let mut compiled = Vec::with_capacity(rule.ops.len());
    if rule.phase.enabled() {
        for op in rule.ops.iter().take(MAX_OPS) {
            let signature = format!(
                "{}|{}",
                rule.id,
                serde_json::to_string(op).unwrap_or_default()
            );
            if let Some(c) = checker.op(op, &signature) {
                compiled.push(c);
            }
        }
    }
    if !checker.issues.is_empty() {
        return Err(checker.issues);
    }
    let sensitive = checker.sensitive;
    Ok((
        ValidRule {
            selector: CompiledSelector::compile(&rule.select),
            when,
            ops: compiled,
            rule,
        },
        sensitive,
    ))
}

/// 校验并编译一个规则集（`Vec<Rule>` 的 JSON 数组）。
pub fn validate(doc: &Value) -> Result<Validated, Vec<Issue>> {
    let doc_issue = |code: &'static str, detail: String| Issue {
        code,
        rule: None,
        index: None,
        detail,
    };
    let Some(items) = doc.as_array() else {
        return Err(vec![doc_issue("not_array", "规则集必须是数组".to_owned())]);
    };
    if items.len() > MAX_RULES {
        return Err(vec![doc_issue(
            "too_many_rules",
            format!("一个规则集最多 {MAX_RULES} 条规则"),
        )]);
    }
    if doc.to_string().len() > MAX_DOC_BYTES {
        return Err(vec![doc_issue(
            "doc_too_large",
            format!("规则文档不能超过 {} KiB", MAX_DOC_BYTES / 1024),
        )]);
    }
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    let mut out = Validated::default();
    for (index, item) in items.iter().enumerate() {
        match validate_rule(index, item) {
            Ok((valid, mut sensitive)) => {
                if !seen.insert(valid.rule.id.clone()) {
                    issues.push(Issue {
                        code: "duplicate_id",
                        rule: Some(valid.rule.id.clone()),
                        index: Some(index),
                        detail: "同一规则集内 id 必须唯一".to_owned(),
                    });
                }
                out.sensitive.append(&mut sensitive);
                out.rules.push(valid);
            }
            Err(mut found) => issues.append(&mut found),
        }
        if issues.len() >= MAX_ISSUES {
            issues.truncate(MAX_ISSUES);
            break;
        }
    }
    if issues.is_empty() {
        Ok(out)
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn codes(doc: &Value) -> Vec<&'static str> {
        match validate(doc) {
            Ok(_) => Vec::new(),
            Err(issues) => issues.into_iter().map(|i| i.code).collect(),
        }
    }

    fn one(op: Value) -> Value {
        let mut rule = serde_json::Map::new();
        rule.insert("id".to_owned(), json!("t"));
        rule.insert("do".to_owned(), Value::Array(vec![op]));
        Value::Array(vec![Value::Object(rule)])
    }

    #[test]
    fn accepts_a_realistic_vendor_rule() {
        let doc = json!([{
            "id": "moonshot.k2-6-temperature-one",
            "select": {"vendor": ["moonshot"], "model": ["kimi-k2.6"], "dialect": ["openai.chat"]},
            "do": [{"op": "clamp", "path": "/temperature", "min": 1, "max": 1}],
            "doc": {"why": "w", "verified_at": "2026-10-24"}
        }]);
        let valid = validate(&doc).expect("valid");
        assert_eq!(valid.len(), 1);
        assert!(valid.sensitive().is_empty(), "{:?}", valid.sensitive());
    }

    #[test]
    fn document_level_limits() {
        assert_eq!(codes(&json!({})), ["not_array"]);
        let many: Vec<Value> = (0..=MAX_RULES)
            .map(|i| json!({"id": format!("r{i}"), "do": [{"op": "remove", "paths": ["/x"]}]}))
            .collect();
        assert_eq!(codes(&Value::Array(many)), ["too_many_rules"]);
        let big = json!([{"id": "a", "do": [{"op": "set", "path": "/x", "value": "y".repeat(MAX_DOC_BYTES)}]}]);
        assert_eq!(codes(&big), ["doc_too_large"]);
        let dup = json!([
            {"id": "a", "do": [{"op": "remove", "paths": ["/x"]}]},
            {"id": "a", "do": [{"op": "remove", "paths": ["/y"]}]}
        ]);
        assert_eq!(codes(&dup), ["duplicate_id"]);
        assert!(validate(&json!([])).expect("empty is fine").is_empty());
    }

    #[test]
    fn rule_shape_errors_name_the_problem() {
        assert_eq!(
            codes(&json!([{"id": "Bad Id", "do": [{"op": "remove", "paths": ["/x"]}]}])),
            ["invalid_id"]
        );
        assert_eq!(codes(&json!([{"id": "a", "do": []}])), ["no_ops"]);
        assert_eq!(
            codes(&json!([{"id": "a", "disabled": true}])),
            Vec::<&str>::new()
        );
        assert_eq!(codes(&one(json!({"op": "teleport"}))), ["unknown_op"]);
        assert_eq!(codes(&one(json!({"paths": ["/x"]}))), ["missing_op"]);
        assert_eq!(
            codes(&json!([{"id": "a", "typo": 1, "do": [{"op": "remove", "paths": ["/x"]}]}])),
            ["invalid_rule"]
        );
        let too_many_ops: Vec<Value> = (0..=MAX_OPS)
            .map(|_| json!({"op": "remove", "paths": ["/x"]}))
            .collect();
        assert_eq!(
            codes(&json!([{"id": "a", "do": too_many_ops}])),
            ["too_many_ops"]
        );
    }

    #[test]
    fn reserved_features_are_refused_with_their_own_codes() {
        let cel =
            json!([{"id": "a", "if": "request.stream", "do": [{"op": "remove", "paths": ["/x"]}]}]);
        assert_eq!(codes(&cel), ["cel_not_enabled"]);
        for phase in ["wire", "request_pre"] {
            let reserved =
                json!([{"id": "a", "phase": phase, "do": [{"op": "remove", "paths": ["/x"]}]}]);
            assert_eq!(codes(&reserved), ["phase_not_enabled"], "{phase}");
        }
    }

    #[test]
    fn selector_values_are_checked() {
        let bad = |select: Value| {
            codes(
                &json!([{"id": "a", "select": select, "do": [{"op": "remove", "paths": ["/x"]}]}]),
            )
        };
        assert_eq!(
            bad(json!({"dialect": ["openai.chatt"]})),
            ["selector_unknown_value"]
        );
        assert_eq!(
            bad(json!({"operation": ["chats"]})),
            ["selector_unknown_value"]
        );
        assert_eq!(bad(json!({"model": [""]})), ["selector_invalid_item"]);
        assert_eq!(bad(json!({"host": ["a b"]})), ["selector_invalid_item"]);
        assert_eq!(
            bad(json!({"vendor": ["De Epseek"]})),
            ["selector_invalid_item", "selector_invalid_item"]
        );
        assert_eq!(
            bad(json!({"vendor": ["builtin:deepseek"], "host": ["*.x.com"], "stream": true})),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn protected_paths_are_refused_for_every_write_op() {
        for path in [
            "/model",
            "/stream",
            "/previous_response_id",
            "/conversation",
            "/provider",
            "/messages",
            "/*/x",
        ] {
            assert_eq!(
                codes(&one(json!({"op": "remove", "paths": [path]}))),
                ["protected_path"],
                "remove {path}"
            );
            assert_eq!(
                codes(&one(json!({"op": "set", "path": path, "value": 1}))),
                ["protected_path"],
                "set {path}"
            );
            assert_eq!(
                codes(&one(json!({"op": "clamp", "path": path, "min": 1}))),
                ["protected_path"],
                "clamp {path}"
            );
        }
        assert_eq!(
            codes(&one(json!({"op": "rename", "from": "/a", "to": "/model"}))),
            ["protected_path"]
        );
        assert_eq!(
            codes(&one(json!({"op": "rename", "from": "/model", "to": "/a"}))),
            ["protected_path"]
        );
        assert_eq!(
            codes(&one(json!({"op": "copy", "from": "/a", "to": "/stream"}))),
            ["protected_path"]
        );
        assert!(
            codes(&one(
                json!({"op": "copy", "from": "/model", "to": "/x_model"})
            ))
            .is_empty(),
            "读受保护路径不算写"
        );
        assert_eq!(
            codes(&one(
                json!({"op": "map_value", "path": "/model", "table": {"a": "b"}})
            )),
            ["protected_path"]
        );
    }

    #[test]
    fn path_errors_are_reported() {
        assert_eq!(
            codes(&one(json!({"op": "remove", "paths": ["no-slash"]}))),
            ["invalid_path"]
        );
        assert_eq!(
            codes(&one(json!({"op": "remove", "paths": []}))),
            ["no_paths"]
        );
        assert_eq!(
            codes(&one(json!({"op": "remove", "paths": ["/a/*"]}))),
            ["wildcard_leaf"]
        );
        assert_eq!(
            codes(&one(json!({"op": "set", "path": "/a/*", "value": 1}))),
            ["wildcard_leaf"]
        );
        assert_eq!(
            codes(&one(json!({"op": "rename", "from": "/a/*/b", "to": "/c"}))),
            ["wildcard_not_allowed"]
        );
        assert_eq!(
            codes(&one(json!({"op": "rename", "from": "/a", "to": "/a/b"}))),
            ["overlapping_paths"]
        );
        let many: Vec<String> = (0..=MAX_PATHS_PER_OP).map(|i| format!("/f{i}")).collect();
        assert_eq!(
            codes(&one(json!({"op": "remove", "paths": many}))),
            ["too_many_paths"]
        );
    }

    #[test]
    fn stream_usage_needs_an_explicit_ack_unless_the_write_hardens_it() {
        let strip = one(json!({"op": "remove", "paths": ["/stream_options"]}));
        assert_eq!(codes(&strip), ["billing_critical_ack_required"]);
        let acked = json!([{"id": "t", "billing_critical_ack": true, "do": [{"op": "remove", "paths": ["/stream_options"]}]}]);
        assert_eq!(codes(&acked), Vec::<&str>::new());
        let harden =
            one(json!({"op": "set", "path": "/stream_options/include_usage", "value": true}));
        assert_eq!(codes(&harden), Vec::<&str>::new());
        let weaken =
            one(json!({"op": "set", "path": "/stream_options/include_usage", "value": false}));
        assert_eq!(codes(&weaken), ["billing_critical_ack_required"]);
        let renamed = one(json!({"op": "rename", "from": "/stream_options", "to": "/so"}));
        assert_eq!(codes(&renamed), ["billing_critical_ack_required"]);
    }

    #[test]
    fn keep_only_without_stream_options_needs_the_ack() {
        let drops = one(json!({"op": "keep_only", "paths": ["/temperature"]}));
        assert_eq!(codes(&drops), ["billing_critical_ack_required"]);
        let keeps = one(json!({"op": "keep_only", "paths": ["/temperature", "/stream_options"]}));
        assert_eq!(codes(&keeps), Vec::<&str>::new());
    }

    #[test]
    fn writing_sensitive_paths_is_reported_but_removing_them_is_free() {
        let write = validate(&one(
            json!({"op": "set", "path": "/service_tier", "value": "flex"}),
        ))
        .expect("valid");
        assert_eq!(write.sensitive().len(), 1);
        assert!(write.sensitive()[0].starts_with("t|"));
        let remove = validate(&one(
            json!({"op": "remove", "paths": ["/service_tier", "/instructions"]}),
        ))
        .expect("valid");
        assert!(remove.sensitive().is_empty(), "{:?}", remove.sensitive());
        let roles = validate(&one(
            json!({"op": "role_map", "from": "developer", "to": "system"}),
        ))
        .expect("valid");
        assert_eq!(roles.sensitive().len(), 2, "messages 与 input 各一");
    }

    #[test]
    fn clamp_bounds_are_checked() {
        assert_eq!(
            codes(&one(json!({"op": "clamp", "path": "/t"}))),
            ["clamp_bounds"]
        );
        assert_eq!(
            codes(&one(
                json!({"op": "clamp", "path": "/t", "min": 2, "max": 1})
            )),
            ["clamp_bounds"]
        );
        assert_eq!(
            codes(&one(
                json!({"op": "clamp", "path": "/t", "min": 1, "max": 1.0})
            )),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn table_and_role_map_shape_checks() {
        assert_eq!(
            codes(&one(json!({"op": "map_value", "path": "/x", "table": {}}))),
            ["invalid_table"]
        );
        assert_eq!(
            codes(&one(json!({"op": "role_map", "from": "a", "to": "a"}))),
            ["invalid_role"]
        );
        assert_eq!(
            codes(&one(json!({"op": "role_map", "from": "", "to": "a"}))),
            ["invalid_role"]
        );
    }

    #[test]
    fn issues_are_capped() {
        let rules: Vec<Value> = (0..MAX_ISSUES + 20)
            .map(|i| json!({"id": format!("r{i}"), "do": [{"op": "remove", "paths": ["/model"]}]}))
            .collect();
        let issues = validate(&Value::Array(rules)).expect_err("invalid");
        assert_eq!(issues.len(), MAX_ISSUES);
    }

    #[test]
    fn to_value_round_trips_through_validation() {
        let doc = json!([
            {"id": "a", "select": {"model": ["x*"]}, "do": [{"op": "set", "path": "/t", "value": 1.0, "mode": "if_absent"}]},
            {"id": "b", "disabled": true}
        ]);
        let valid = validate(&doc).expect("valid");
        let again = validate(&valid.to_value()).expect("again");
        assert_eq!(valid.to_value(), again.to_value());
        assert!(valid.to_value().to_string().contains("1.0"));
    }
}
