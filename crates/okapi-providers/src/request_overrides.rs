//! 渠道级声明式请求改写规则（`channels.settings.request_overrides`，借鉴 new-api `param_override` / `header_override`）。
//!
//! 补齐简单顶层 `strip_request_fields` / `inject_request_fields` / 静态 `extra_headers` 的表达边界：
//! 支持嵌套 JSON 路径、条件过滤（模型 / 入口协议 / UA / 请求头 / 请求体字段）、高频 9 种动作
//! （`set` / `delete` / `move` / `copy` / `replace` / `regex_replace` / `set_header` / `delete_header` / `return_error`）。
//!
//! 安全护栏：
//! - 受保护体根键（`model` / `messages` / `input` / `contents` / `system` / `stream` / `provider` /
//!   `previous_response_id` / `conversation`）禁止改写；敏感对话内容路径也禁止作为 `copy` 源或条件读取，
//!   防止管理员借规则篡改计费模型、破坏会话串联或旁路窥探用户消息内容。
//! - 受保护 HTTP 头（鉴权、逐跳、Host、Content-*、x-okapi-request-id）复用 [`crate::http::is_forbidden_header`] 拦截。
//! - 规则数 / 条件数 / 总字节数 / 嵌套深度 / 正则编译尺寸均有界，写入时即校验；空配置热路径零开销。

use crate::http::is_forbidden_header;
use okapi_api::contracts::{
    OverrideConditionDto, OverrideMatcherDto, OverrideOpDto, RequestOverrideRuleDto,
};
use regex::{Regex, RegexBuilder};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{Map, Value};

pub const MAX_RULES: usize = 32;
pub const MAX_CONDITIONS_PER_RULE: usize = 8;
pub const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
pub const MAX_PATH_DEPTH: usize = 8;
pub const MAX_REGEX_LEN: usize = 256;
pub const REGEX_SIZE_LIMIT: usize = 64 * 1024;
pub const MAX_ERROR_REASON_LEN: usize = 128;

/// `UpstreamError::Build` 承载 `return_error` 拒绝的前缀（`override_reject:<status>:<reason>`）。
pub const OVERRIDE_REJECT_PREFIX: &str = "override_reject:";

/// 禁止作为写入 / 删除 / 移动目标的顶层体字段：保护计费、会话串联与提示词原文。
const PROTECTED_WRITE_ROOTS: &[&str] = &[
    "model",
    "messages",
    "input",
    "contents",
    "system",
    "stream",
    "provider",
    "previous_response_id",
    "conversation",
];

/// 禁止作为 `copy` 源或 `body:` 条件读取的敏感对话正文路径。
const SENSITIVE_READ_ROOTS: &[&str] = &["messages", "input", "contents", "system"];

/// 规则求值上下文（全部借用，热路径零克隆）。
pub struct OverrideContext<'a> {
    pub model: &'a str,
    pub requested_model: &'a str,
    pub upstream_model: &'a str,
    pub ingress: &'a str,
    pub user_agent: Option<&'a str>,
    pub client_headers: &'a [(String, String)],
}

/// 规则执行产出的出站 Header 增量补丁。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverrideHeaderPatch {
    pub set: Vec<(String, String)>,
    pub delete: Vec<String>,
}

impl OverrideHeaderPatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.delete.is_empty()
    }

    /// 将补丁应用到出站 `extra_headers`（先按大小写不敏感移除，再覆盖写入；受保护头永远跳过）。
    pub fn apply_to(&self, headers: &mut Vec<(String, String)>) {
        if self.is_empty() {
            return;
        }
        for name in &self.delete {
            if is_forbidden_header(name) {
                continue;
            }
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        }
        for (name, val) in &self.set {
            if is_forbidden_header(name)
                || HeaderName::from_bytes(name.as_bytes()).is_err()
                || HeaderValue::from_str(val).is_err()
            {
                continue;
            }
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            headers.push((name.clone(), val.clone()));
        }
    }
}

/// 规则执行结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverrideOutcome {
    pub body_changed: bool,
    pub headers: OverrideHeaderPatch,
}

/// `return_error` 命中后的短路拒绝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideRejection {
    pub status: u16,
    pub reason: String,
}

impl OverrideRejection {
    /// 编码为 `UpstreamError::Build` 载荷。
    #[must_use]
    pub fn encode_build_reason(&self) -> String {
        format!("{OVERRIDE_REJECT_PREFIX}{}:{}", self.status, self.reason)
    }

    /// 从 `UpstreamError::Build` 载荷解码。
    #[must_use]
    pub fn decode_build_reason(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix(OVERRIDE_REJECT_PREFIX)?;
        let (status_str, reason) = rest.split_once(':')?;
        let status: u16 = status_str.parse().ok()?;
        if !(400..=599).contains(&status) {
            return None;
        }
        let reason = if reason.is_empty() {
            "override_rejected".to_owned()
        } else {
            reason.to_owned()
        };
        Some(Self { status, reason })
    }
}

/// 写入端完整校验 `settings.request_overrides`。
/// `null` 返回空表；任何非法字段、越界数量、受保护路径或坏正则立即拒掉。
pub fn validate_rules_value(value: &Value) -> Result<Vec<RequestOverrideRuleDto>, &'static str> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if !value.is_array() {
        return Err("request_overrides");
    }
    if serde_json::to_vec(value)
        .ok()
        .is_none_or(|buf| buf.len() > MAX_PAYLOAD_BYTES)
    {
        return Err("request_overrides");
    }
    let rules: Vec<RequestOverrideRuleDto> =
        serde_json::from_value(value.clone()).map_err(|_| "request_overrides")?;
    if rules.len() > MAX_RULES {
        return Err("request_overrides");
    }
    for rule in &rules {
        validate_rule(rule)?;
    }
    Ok(rules)
}

/// 热路径解析：若 `value` 为 `Null` 或空数组返回 `None`（零开销跳过）。
#[must_use]
pub fn parse_rules_runtime(value: &Value) -> Option<Vec<RequestOverrideRuleDto>> {
    let arr = value.as_array()?;
    if arr.is_empty() {
        return None;
    }
    serde_json::from_value::<Vec<RequestOverrideRuleDto>>(value.clone())
        .ok()
        .filter(|rules| !rules.is_empty())
}

fn validate_rule(rule: &RequestOverrideRuleDto) -> Result<(), &'static str> {
    if let Some(name) = &rule.name
        && (name.len() > 128 || name.chars().any(char::is_control))
    {
        return Err("request_overrides");
    }
    if rule.conditions.len() > MAX_CONDITIONS_PER_RULE {
        return Err("request_overrides");
    }
    for cond in &rule.conditions {
        validate_condition(cond)?;
    }
    match rule.op {
        OverrideOpDto::Set => {
            validate_body_path(&rule.path, true)?;
            if rule.value.is_none() {
                return Err("request_overrides");
            }
        }
        OverrideOpDto::Delete => {
            validate_body_path(&rule.path, true)?;
        }
        OverrideOpDto::Move => {
            let from = rule.from.as_deref().ok_or("request_overrides")?;
            validate_body_path(from, true)?;
            validate_body_path(&rule.path, true)?;
        }
        OverrideOpDto::Copy => {
            let from = rule.from.as_deref().ok_or("request_overrides")?;
            validate_read_body_path(from)?;
            validate_body_path(&rule.path, true)?;
        }
        OverrideOpDto::Replace => {
            validate_body_path(&rule.path, true)?;
            let pat = rule.pattern.as_deref().ok_or("request_overrides")?;
            if pat.is_empty() || pat.len() > MAX_REGEX_LEN {
                return Err("request_overrides");
            }
            if rule.value.as_ref().is_some_and(|v| !v.is_string()) {
                return Err("request_overrides");
            }
        }
        OverrideOpDto::RegexReplace => {
            validate_body_path(&rule.path, true)?;
            let pat = rule.pattern.as_deref().ok_or("request_overrides")?;
            compile_bounded_regex(pat)?;
            if rule.value.as_ref().is_some_and(|v| !v.is_string()) {
                return Err("request_overrides");
            }
        }
        OverrideOpDto::SetHeader => {
            validate_header_name(&rule.path)?;
            let val = rule
                .value
                .as_ref()
                .and_then(Value::as_str)
                .ok_or("request_overrides")?;
            if HeaderValue::from_str(val).is_err() {
                return Err("request_overrides");
            }
        }
        OverrideOpDto::DeleteHeader => {
            validate_header_name(&rule.path)?;
        }
        OverrideOpDto::ReturnError => {
            let status = rule.status.unwrap_or(400);
            if !(400..=599).contains(&status) {
                return Err("request_overrides");
            }
            if let Some(val) = &rule.value {
                let Some(reason) = val.as_str() else {
                    return Err("request_overrides");
                };
                if reason.len() > MAX_ERROR_REASON_LEN || reason.chars().any(char::is_control) {
                    return Err("request_overrides");
                }
            }
        }
    }
    Ok(())
}

fn validate_condition(cond: &OverrideConditionDto) -> Result<(), &'static str> {
    let field = cond.field.trim();
    if field.is_empty() || field.len() > 160 || field.chars().any(char::is_control) {
        return Err("request_overrides");
    }
    if matches!(
        field,
        "model" | "requested_model" | "upstream_model" | "ingress" | "user_agent"
    ) {
        // valid built-in context field
    } else if let Some(header_name) = field.strip_prefix("header:") {
        let name = header_name.trim();
        if name.is_empty()
            || is_forbidden_header(name)
            || HeaderName::from_bytes(name.as_bytes()).is_err()
        {
            return Err("request_overrides");
        }
    } else if let Some(body_path) = field.strip_prefix("body:") {
        validate_read_body_path(body_path)?;
    } else {
        return Err("request_overrides");
    }

    match cond.matcher {
        OverrideMatcherDto::Exists | OverrideMatcherDto::Missing => Ok(()),
        OverrideMatcherDto::Regex => {
            let pat = cond
                .value
                .as_ref()
                .and_then(Value::as_str)
                .ok_or("request_overrides")?;
            compile_bounded_regex(pat)?;
            Ok(())
        }
        OverrideMatcherDto::Gt
        | OverrideMatcherDto::Gte
        | OverrideMatcherDto::Lt
        | OverrideMatcherDto::Lte => {
            if cond.value.as_ref().and_then(value_as_f64).is_none() {
                return Err("request_overrides");
            }
            Ok(())
        }
        OverrideMatcherDto::Eq
        | OverrideMatcherDto::Neq
        | OverrideMatcherDto::Prefix
        | OverrideMatcherDto::Suffix
        | OverrideMatcherDto::Contains => {
            if cond.value.is_none() {
                return Err("request_overrides");
            }
            Ok(())
        }
    }
}

fn validate_header_name(raw: &str) -> Result<(), &'static str> {
    let name = raw.trim();
    if name.is_empty()
        || is_forbidden_header(name)
        || HeaderName::from_bytes(name.as_bytes()).is_err()
    {
        return Err("request_overrides");
    }
    Ok(())
}

fn split_path(path: &str) -> Result<Vec<&str>, &'static str> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.len() > 160 {
        return Err("request_overrides");
    }
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.is_empty() || parts.len() > MAX_PATH_DEPTH {
        return Err("request_overrides");
    }
    for seg in &parts {
        if seg.is_empty() || seg.chars().any(char::is_control) {
            return Err("request_overrides");
        }
    }
    Ok(parts)
}

fn validate_body_path(path: &str, check_protected_write: bool) -> Result<(), &'static str> {
    let parts = split_path(path)?;
    if check_protected_write && PROTECTED_WRITE_ROOTS.contains(&parts[0]) {
        return Err("request_overrides");
    }
    Ok(())
}

fn validate_read_body_path(path: &str) -> Result<(), &'static str> {
    let parts = split_path(path)?;
    if SENSITIVE_READ_ROOTS.contains(&parts[0]) {
        return Err("request_overrides");
    }
    Ok(())
}

fn compile_bounded_regex(pattern: &str) -> Result<Regex, &'static str> {
    if pattern.is_empty() || pattern.len() > MAX_REGEX_LEN {
        return Err("request_overrides");
    }
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|_| "request_overrides")
}

/// 按序执行规则集：每条启用且全部条件命中的规则立即作用于 `body` / `headers`；
/// 命中 `return_error` 立刻短路返回 `Err(OverrideRejection)`。
pub fn apply_rules(
    rules: &[RequestOverrideRuleDto],
    ctx: &OverrideContext<'_>,
    body: &mut Value,
) -> Result<OverrideOutcome, OverrideRejection> {
    let mut outcome = OverrideOutcome::default();
    for rule in rules {
        if !rule.enabled {
            continue;
        }
        if !rule
            .conditions
            .iter()
            .all(|cond| eval_condition(cond, ctx, body))
        {
            continue;
        }
        match rule.op {
            OverrideOpDto::Set => {
                if let (Ok(parts), Some(val)) = (split_path(&rule.path), rule.value.as_ref())
                    && !PROTECTED_WRITE_ROOTS.contains(&parts[0])
                    && set_path(body, &parts, val.clone())
                {
                    outcome.body_changed = true;
                }
            }
            OverrideOpDto::Delete => {
                if let Ok(parts) = split_path(&rule.path)
                    && !PROTECTED_WRITE_ROOTS.contains(&parts[0])
                    && delete_path(body, &parts).is_some()
                {
                    outcome.body_changed = true;
                }
            }
            OverrideOpDto::Move => {
                if let (Some(from_raw), Ok(to_parts)) = (rule.from.as_deref(), split_path(&rule.path))
                    && let Ok(from_parts) = split_path(from_raw)
                    && !PROTECTED_WRITE_ROOTS.contains(&from_parts[0])
                    && !PROTECTED_WRITE_ROOTS.contains(&to_parts[0])
                    && let Some(moved) = delete_path(body, &from_parts)
                {
                    let _ = set_path(body, &to_parts, moved);
                    outcome.body_changed = true;
                }
            }
            OverrideOpDto::Copy => {
                if let (Some(from_raw), Ok(to_parts)) = (rule.from.as_deref(), split_path(&rule.path))
                    && let Ok(from_parts) = split_path(from_raw)
                    && !SENSITIVE_READ_ROOTS.contains(&from_parts[0])
                    && !PROTECTED_WRITE_ROOTS.contains(&to_parts[0])
                    && let Some(cloned) = get_path(body, &from_parts).cloned()
                    && set_path(body, &to_parts, cloned)
                {
                    outcome.body_changed = true;
                }
            }
            OverrideOpDto::Replace => {
                if let (Ok(parts), Some(pat)) = (split_path(&rule.path), rule.pattern.as_deref())
                    && !PROTECTED_WRITE_ROOTS.contains(&parts[0])
                    && !pat.is_empty()
                {
                    let replacement = rule.value.as_ref().and_then(Value::as_str).unwrap_or("");
                    if let Some(target) = get_path_mut(body, &parts)
                        && let Some(text) = target.as_str()
                    {
                        let next = text.replace(pat, replacement);
                        if next != text {
                            *target = Value::String(next);
                            outcome.body_changed = true;
                        }
                    }
                }
            }
            OverrideOpDto::RegexReplace => {
                if let (Ok(parts), Some(pat)) = (split_path(&rule.path), rule.pattern.as_deref())
                    && !PROTECTED_WRITE_ROOTS.contains(&parts[0])
                    && let Ok(re) = compile_bounded_regex(pat)
                {
                    let replacement = rule.value.as_ref().and_then(Value::as_str).unwrap_or("");
                    if let Some(target) = get_path_mut(body, &parts)
                        && let Some(text) = target.as_str()
                    {
                        let next = re.replace_all(text, replacement);
                        if next.as_ref() != text {
                            *target = Value::String(next.into_owned());
                            outcome.body_changed = true;
                        }
                    }
                }
            }
            OverrideOpDto::SetHeader => {
                let name = rule.path.trim();
                if let Some(val) = rule.value.as_ref().and_then(Value::as_str)
                    && !is_forbidden_header(name)
                    && HeaderName::from_bytes(name.as_bytes()).is_ok()
                    && HeaderValue::from_str(val).is_ok()
                {
                    outcome
                        .headers
                        .set
                        .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
                    outcome.headers.set.push((name.to_owned(), val.to_owned()));
                }
            }
            OverrideOpDto::DeleteHeader => {
                let name = rule.path.trim();
                if !is_forbidden_header(name) && HeaderName::from_bytes(name.as_bytes()).is_ok() {
                    outcome
                        .headers
                        .set
                        .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
                    if !outcome
                        .headers
                        .delete
                        .iter()
                        .any(|existing| existing.eq_ignore_ascii_case(name))
                    {
                        outcome.headers.delete.push(name.to_owned());
                    }
                }
            }
            OverrideOpDto::ReturnError => {
                let status = rule
                    .status
                    .filter(|s| (400..=599).contains(s))
                    .unwrap_or(400);
                let reason = rule
                    .value
                    .as_ref()
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.len() <= MAX_ERROR_REASON_LEN)
                    .unwrap_or("override_rejected")
                    .to_owned();
                return Err(OverrideRejection { status, reason });
            }
        }
    }
    Ok(outcome)
}

fn eval_condition(cond: &OverrideConditionDto, ctx: &OverrideContext<'_>, body: &Value) -> bool {
    let actual: Option<Value> = match cond.field.trim() {
        "model" => Some(Value::String(ctx.model.to_owned())),
        "requested_model" => Some(Value::String(ctx.requested_model.to_owned())),
        "upstream_model" => Some(Value::String(ctx.upstream_model.to_owned())),
        "ingress" => Some(Value::String(ctx.ingress.to_owned())),
        "user_agent" => ctx.user_agent.map(|ua| Value::String(ua.to_owned())),
        other => {
            if let Some(header_name) = other.strip_prefix("header:") {
                let target = header_name.trim();
                if is_forbidden_header(target) {
                    return false;
                }
                ctx.client_headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(target))
                    .map(|(_, v)| Value::String(v.clone()))
                    .or_else(|| {
                        target
                            .eq_ignore_ascii_case("user-agent")
                            .then_some(ctx.user_agent)
                            .flatten()
                            .map(|ua| Value::String(ua.to_owned()))
                    })
            } else if let Some(body_path) = other.strip_prefix("body:") {
                split_path(body_path)
                    .ok()
                    .filter(|parts| !SENSITIVE_READ_ROOTS.contains(&parts[0]))
                    .and_then(|parts| get_path(body, &parts).cloned())
            } else {
                None
            }
        }
    };

    match cond.matcher {
        OverrideMatcherDto::Exists => actual.is_some_and(|v| !v.is_null()),
        OverrideMatcherDto::Missing => actual.is_none_or(|v| v.is_null()),
        OverrideMatcherDto::Eq => match (&actual, &cond.value) {
            (Some(a), Some(b)) => values_equal(a, b),
            _ => false,
        },
        OverrideMatcherDto::Neq => match (&actual, &cond.value) {
            (Some(a), Some(b)) => !values_equal(a, b),
            (None, Some(_)) => true,
            _ => false,
        },
        OverrideMatcherDto::Prefix => match (actual.as_ref().and_then(value_as_string), cond.value.as_ref().and_then(value_as_string)) {
            (Some(a), Some(b)) => a.starts_with(b.as_str()),
            _ => false,
        },
        OverrideMatcherDto::Suffix => match (actual.as_ref().and_then(value_as_string), cond.value.as_ref().and_then(value_as_string)) {
            (Some(a), Some(b)) => a.ends_with(b.as_str()),
            _ => false,
        },
        OverrideMatcherDto::Contains => match (actual.as_ref().and_then(value_as_string), cond.value.as_ref().and_then(value_as_string)) {
            (Some(a), Some(b)) => a.contains(b.as_str()),
            _ => false,
        },
        OverrideMatcherDto::Regex => {
            let Some(a) = actual.as_ref().and_then(value_as_string) else {
                return false;
            };
            let Some(pat) = cond.value.as_ref().and_then(Value::as_str) else {
                return false;
            };
            compile_bounded_regex(pat).is_ok_and(|re| re.is_match(&a))
        }
        OverrideMatcherDto::Gt
        | OverrideMatcherDto::Gte
        | OverrideMatcherDto::Lt
        | OverrideMatcherDto::Lte => {
            let (Some(a), Some(b)) = (
                actual.as_ref().and_then(value_as_f64),
                cond.value.as_ref().and_then(value_as_f64),
            ) else {
                return false;
            };
            match cond.matcher {
                OverrideMatcherDto::Gt => a > b,
                OverrideMatcherDto::Gte => a >= b,
                OverrideMatcherDto::Lt => a < b,
                OverrideMatcherDto::Lte => a <= b,
                _ => false,
            }
        }
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    match (value_as_f64(a), value_as_f64(b)) {
        (Some(fa), Some(fb)) if a.is_number() || b.is_number() => (fa - fb).abs() < f64::EPSILON,
        _ => false,
    }
}

fn value_as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|x| x.is_finite()),
        _ => None,
    }
}

fn get_path<'a>(root: &'a Value, parts: &[&str]) -> Option<&'a Value> {
    let mut cur = root;
    for seg in parts {
        cur = cur.as_object()?.get(*seg)?;
    }
    Some(cur)
}

fn get_path_mut<'a>(root: &'a mut Value, parts: &[&str]) -> Option<&'a mut Value> {
    let mut cur = root;
    for seg in parts {
        cur = cur.as_object_mut()?.get_mut(*seg)?;
    }
    Some(cur)
}

fn set_path(root: &mut Value, parts: &[&str], value: Value) -> bool {
    let Some((last, parents)) = parts.split_last() else {
        return false;
    };
    let mut cur = root;
    for seg in parents {
        let Some(obj) = cur.as_object_mut() else {
            return false;
        };
        let entry = obj
            .entry((*seg).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cur = entry;
    }
    let Some(obj) = cur.as_object_mut() else {
        return false;
    };
    obj.insert((*last).to_owned(), value);
    true
}

fn delete_path(root: &mut Value, parts: &[&str]) -> Option<Value> {
    let (last, parents) = parts.split_last()?;
    let mut cur = root;
    for seg in parents {
        cur = cur.as_object_mut()?.get_mut(*seg)?;
    }
    cur.as_object_mut()?.remove(*last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx<'a>() -> OverrideContext<'a> {
        OverrideContext {
            model: "gpt-5",
            requested_model: "gpt-5",
            upstream_model: "gpt-5-2026",
            ingress: "openai",
            user_agent: Some("MyAgent/2.1"),
            client_headers: &[],
        }
    }

    #[test]
    fn validates_and_rejects_protected_paths_and_forbidden_headers() {
        assert!(validate_rules_value(&Value::Null).unwrap().is_empty());
        assert!(validate_rules_value(&json!([])).unwrap().is_empty());

        for bad in [
            json!([{"op":"set","path":"model","value":"other"}]),
            json!([{"op":"delete","path":"messages"}]),
            json!([{"op":"set","path":"system.prompt","value":"hack"}]),
            json!([{"op":"copy","from":"messages","path":"extra"}]),
            json!([{"op":"move","from":"stream","path":"flag"}]),
            json!([{"op":"set_header","path":"Authorization","value":"Bearer x"}]),
            json!([{"op":"delete_header","path":"x-api-key"}]),
            json!([{"op":"regex_replace","path":"user","pattern":"[invalid","value":"x"}]),
            json!([{"op":"return_error","status":200,"value":"nope"}]),
            json!([{"op":"set","path":"temperature","value":0.5,"conditions":[{"field":"body:messages","matcher":"exists"}]}]),
        ] {
            assert!(validate_rules_value(&bad).is_err(), "must reject: {bad}");
        }
    }

    #[test]
    fn applies_nested_body_ops_conditions_and_headers() {
        let rules = validate_rules_value(&json!([
            {
                "name": "cap temperature for gpt-5",
                "enabled": true,
                "conditions": [
                    {"field": "model", "matcher": "prefix", "value": "gpt-5"},
                    {"field": "body:temperature", "matcher": "gt", "value": 0.7}
                ],
                "op": "set",
                "path": "temperature",
                "value": 0.7
            },
            {
                "op": "set",
                "path": "reasoning.effort",
                "value": "medium"
            },
            {
                "op": "move",
                "from": "max_tokens",
                "path": "max_completion_tokens"
            },
            {
                "op": "copy",
                "from": "user",
                "path": "metadata.caller"
            },
            {
                "op": "regex_replace",
                "path": "metadata.caller",
                "pattern": "^usr_(\\d+)$",
                "value": "id-$1"
            },
            {
                "op": "set_header",
                "path": "X-Tenant-Tier",
                "value": "gold",
                "conditions": [{"field": "user_agent", "matcher": "contains", "value": "MyAgent"}]
            },
            {
                "op": "delete_header",
                "path": "X-Remove-Me"
            }
        ]))
        .expect("valid rules");

        let mut body = json!({
            "model": "gpt-5",
            "temperature": 1.2,
            "max_tokens": 2048,
            "user": "usr_42"
        });
        let outcome = apply_rules(&rules, &ctx(), &mut body).expect("succeeds");
        assert!(outcome.body_changed);
        assert_eq!(body["temperature"], json!(0.7));
        assert_eq!(body["reasoning"]["effort"], json!("medium"));
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], json!(2048));
        assert_eq!(body["metadata"]["caller"], json!("id-42"));

        let mut headers = vec![("X-Remove-Me".to_owned(), "1".to_owned())];
        outcome.headers.apply_to(&mut headers);
        assert_eq!(headers, vec![("X-Tenant-Tier".to_owned(), "gold".to_owned())]);
    }

    #[test]
    fn return_error_short_circuits_and_roundtrips_reason() {
        let rules = validate_rules_value(&json!([
            {
                "conditions": [{"field": "user_agent", "matcher": "contains", "value": "MyAgent"}],
                "op": "return_error",
                "status": 403,
                "value": "client_blocked"
            },
            {
                "op": "set",
                "path": "temperature",
                "value": 0
            }
        ]))
        .unwrap();
        let mut body = json!({"model": "gpt-5", "temperature": 1});
        let err = apply_rules(&rules, &ctx(), &mut body).unwrap_err();
        assert_eq!(err.status, 403);
        assert_eq!(err.reason, "client_blocked");
        assert_eq!(body["temperature"], json!(1), "later rules must not run");
        let encoded = err.encode_build_reason();
        assert_eq!(OverrideRejection::decode_build_reason(&encoded), Some(err));
    }
}
