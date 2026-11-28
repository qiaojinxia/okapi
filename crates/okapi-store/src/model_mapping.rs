//! 渠道级模型名映射（精确 + 通配符 `*` + 正则 `re:...`，借鉴 new-api 模型映射能力）。
//!
//! 匹配优先级（确定性、无歧义）：
//! 1. **精确命中**：`mapping[model]` 直接借用返回，热路径零分配零正则；
//! 2. **通配符规则**（键含 `*` 且不以 `re:` 开头）：按固定字面量字符数降序（越具体越先命中），
//!    再按字典序排；目标中的 `*` 按顺序展开为各通配捕获段（也支持 `$1`/`$2` 引用）；
//! 3. **正则规则**（键以 `re:` 开头）：按正则模式长度降序、再按字典序排；使用有界编译正则
//!    （`size_limit = 64 KiB`）执行标准 `$1`/`$2`/`${name}` 模板替换；
//! 4. **未命中**：原样借用返回输入模型名。

use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value};
use std::borrow::Cow;

pub const MAX_ENTRIES: usize = 512;
pub const MAX_KEY_LEN: usize = 128;
pub const MAX_TARGET_LEN: usize = 128;
pub const REGEX_PREFIX: &str = "re:";
pub const REGEX_SIZE_LIMIT: usize = 64 * 1024;

/// 写入端校验并规整 `model_mapping`：
/// - 必须是 JSON 对象，最多 512 条；
/// - 键值均须为非空、无控制字符、<= 128 字节的字符串（去首尾空白）；
/// - 以 `re:` 开头的键必须能通过有界正则编译（防 ReDoS / 无效语法入库）。
pub fn validate_and_normalize(value: &Value) -> Result<Value, &'static str> {
    let Some(map) = value.as_object() else {
        return Err("model_mapping");
    };
    if map.len() > MAX_ENTRIES {
        return Err("model_mapping");
    }
    let mut out = Map::with_capacity(map.len());
    for (k, v) in map {
        let key = k.trim();
        let Some(target) = v.as_str().map(str::trim) else {
            return Err("model_mapping");
        };
        if key.is_empty()
            || target.is_empty()
            || key.len() > MAX_KEY_LEN
            || target.len() > MAX_TARGET_LEN
            || key.chars().any(char::is_control)
            || target.chars().any(char::is_control)
        {
            return Err("model_mapping");
        }
        if let Some(pat) = key.strip_prefix(REGEX_PREFIX) {
            compile_bounded_regex(pat)?;
        }
        out.insert(key.to_owned(), Value::String(target.to_owned()));
    }
    Ok(Value::Object(out))
}

/// 解析请求模型名对应的上游实际模型名。
/// 精确命中或空表时零分配直接借用返回；通配/正则命中时返回展开后的新字符串。
#[must_use]
pub fn resolve<'a>(mapping: &'a Value, model: &'a str) -> Cow<'a, str> {
    let Some(map) = mapping.as_object() else {
        return Cow::Borrowed(model);
    };
    if map.is_empty() {
        return Cow::Borrowed(model);
    }

    // 1. 精确命中（O(1)，零分配）
    if let Some(exact) = map
        .get(model)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Cow::Borrowed(exact);
    }

    // 2. 通配符规则（含 `*` 且非 `re:`），按固定字面字符数降序（更具体优先）+ 键字典序
    let mut wildcards: Vec<(&str, &str)> = map
        .iter()
        .filter_map(|(k, v)| {
            if k.starts_with(REGEX_PREFIX) || !k.contains('*') {
                return None;
            }
            let target = v.as_str()?.trim();
            (!target.is_empty()).then_some((k.as_str(), target))
        })
        .collect();
    if !wildcards.is_empty() {
        wildcards.sort_by(|a, b| {
            let lit_a = a.0.bytes().filter(|&c| c != b'*').count();
            let lit_b = b.0.bytes().filter(|&c| c != b'*').count();
            lit_b.cmp(&lit_a).then_with(|| a.0.cmp(b.0))
        });
        for (pattern, target) in wildcards {
            if let Some(mapped) = apply_wildcard(pattern, target, model) {
                return Cow::Owned(mapped);
            }
        }
    }

    // 3. 正则规则（`re:` 前缀），按模式长度降序 + 键字典序
    let mut regexes: Vec<(&str, &str)> = map
        .iter()
        .filter_map(|(k, v)| {
            let pat = k.strip_prefix(REGEX_PREFIX)?;
            if pat.is_empty() {
                return None;
            }
            let target = v.as_str()?.trim();
            (!target.is_empty()).then_some((pat, target))
        })
        .collect();
    if !regexes.is_empty() {
        regexes.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
        for (pat, target) in regexes {
            if let Ok(re) = compile_bounded_regex(pat)
                && re.is_match(model)
            {
                let replaced = re.replace(model, target);
                let trimmed = replaced.trim();
                if !trimmed.is_empty() {
                    return Cow::Owned(trimmed.to_owned());
                }
            }
        }
    }

    Cow::Borrowed(model)
}

fn compile_bounded_regex(pattern: &str) -> Result<Regex, &'static str> {
    if pattern.is_empty() || pattern.len() > MAX_KEY_LEN {
        return Err("model_mapping");
    }
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|_| "model_mapping")
}

/// 通配符匹配与展开：把模式里的 `*` 依次转为捕获组 `(.*)` 全串匹配，
/// 目标里的 `*` 按序替换为对应捕获段（无 `*` 时也支持 `$1`/`$2` 反向引用）。
fn apply_wildcard(pattern: &str, target: &str, input: &str) -> Option<String> {
    let mut regex_src = String::with_capacity(pattern.len() * 2 + 4);
    regex_src.push('^');
    let mut wild_count = 0usize;
    for ch in pattern.chars() {
        if ch == '*' {
            regex_src.push_str("(.*)");
            wild_count += 1;
        } else {
            regex_src.push_str(&regex::escape(&ch.to_string()));
        }
    }
    regex_src.push('$');
    let re = RegexBuilder::new(&regex_src)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .ok()?;
    let caps = re.captures(input)?;
    if target.contains('*') {
        let mut out = String::with_capacity(target.len() + input.len());
        let mut idx = 1usize;
        for ch in target.chars() {
            if ch == '*' && idx <= wild_count {
                if let Some(m) = caps.get(idx) {
                    out.push_str(m.as_str());
                }
                idx += 1;
            } else {
                out.push(ch);
            }
        }
        let trimmed = out.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    if target.contains('$') {
        let mut out = String::new();
        caps.expand(target, &mut out);
        let trimmed = out.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    Some(target.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_match_precedes_wildcard_and_regex() {
        let map = json!({
            "claude-sonnet-4": "exact-override",
            "claude-*": "us.anthropic.claude-*-v1:0",
            "re:^claude-(.+)$": "regex-$1"
        });
        assert_eq!(resolve(&map, "claude-sonnet-4"), "exact-override");
        assert_eq!(
            resolve(&map, "claude-haiku-4"),
            "us.anthropic.claude-haiku-4-v1:0"
        );
    }

    #[test]
    fn wildcards_prefer_more_specific_patterns_and_support_strip_or_prefix() {
        let map = json!({
            "*": "openai/*",
            "anthropic/*": "*",
            "claude-3-*": "bedrock.claude-3-$1"
        });
        assert_eq!(resolve(&map, "gpt-5"), "openai/gpt-5");
        assert_eq!(resolve(&map, "anthropic/claude-opus-4"), "claude-opus-4");
        assert_eq!(resolve(&map, "claude-3-haiku"), "bedrock.claude-3-haiku");
    }

    #[test]
    fn regex_patterns_expand_capture_groups_and_reject_invalid_regex() {
        let map = json!({
            "re:^gpt-(\\d+)(?:-mini)?$": "azure-gpt-$1"
        });
        assert!(validate_and_normalize(&map).is_ok());
        assert_eq!(resolve(&map, "gpt-5-mini"), "azure-gpt-5");
        assert_eq!(resolve(&map, "other-model"), "other-model");

        assert!(validate_and_normalize(&json!({"re:[unclosed": "x"})).is_err());
        assert!(validate_and_normalize(&json!({"re:": "x"})).is_err());
        assert!(validate_and_normalize(&json!({"a": 123})).is_err());
    }
}
