//! 跨方言转换会丢的工具。各方言之间的转换只搬普通函数工具：Responses 的 `custom` 与内置工具、
//! Chat 的 `custom`、Anthropic 带 `type` 的客户端工具（bash / text_editor / computer / memory）、
//! Gemini 的 googleSearch / codeExecution 等在对面都没有对应物，转换时被丢掉。
//!
//! 丢掉的后果分两种：上游托管的工具（搜索、代码解释器、取图、远程 MCP）丢了，模型照样能答，
//! 只是少一项能力；客户端执行的工具（Codex 的 apply_patch 是 `custom`、本地 shell、computer use、
//! Anthropic 的 bash / text_editor）丢了，agent 改不了文件、跑不了命令，而模型浑然不知——
//! 这是最难排查的一类「降智」。渠道按 [`ToolLossPolicy`] 决定拒绝还是丢弃。
use crate::registry::ApiFormat;
use serde_json::Value;

/// 转换后会丢的工具类型（去重、保持请求里的先后）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignTools {
    /// 上游托管：丢了模型照样能答。
    pub hosted: Vec<String>,
    /// 客户端执行：丢了 agent 就没法干活。
    pub client: Vec<String>,
}

impl ForeignTools {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hosted.is_empty() && self.client.is_empty()
    }

    /// 全部类型（托管在前），给错误参数与诊断用。
    #[must_use]
    pub fn all(&self) -> Vec<String> {
        self.hosted.iter().chain(&self.client).cloned().collect()
    }

    /// 去掉另有管线原样带过转换的类型（网关的原生工具准入会把它们装回上游体）。
    #[must_use]
    pub fn without(&self, carried: &[&str]) -> Self {
        let keep = |list: &[String]| -> Vec<String> {
            list.iter()
                .filter(|k| !carried.contains(&k.as_str()))
                .cloned()
                .collect()
        };
        Self {
            hosted: keep(&self.hosted),
            client: keep(&self.client),
        }
    }

    fn push(&mut self, kind: &str, hosted: bool) {
        let list = if hosted {
            &mut self.hosted
        } else {
            &mut self.client
        };
        if !list.iter().any(|k| k == kind) {
            list.push(kind.to_owned());
        }
    }
}

/// 渠道的 `settings.tool_loss_policy`：跨方言转换会丢工具时怎么办。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolLossPolicy {
    /// 缺省：丢托管工具（记进诊断），会丢客户端工具就不走这条渠道。
    #[default]
    Safe,
    /// 会丢任何工具就不走这条渠道。
    Strict,
    /// 全部丢掉照常转发（此前的行为）。
    Allow,
}

impl ToolLossPolicy {
    pub const VALUES: [&'static str; 3] = ["safe", "strict", "allow"];

    /// 未配置或取值不认识按缺省 `safe`（写入时控制台已校验取值）。
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("strict") => Self::Strict,
            Some("allow") => Self::Allow,
            _ => Self::Safe,
        }
    }

    /// 这些工具会被丢时，这条渠道还能不能用。
    #[must_use]
    pub fn admits(self, tools: &ForeignTools) -> bool {
        match self {
            Self::Allow => true,
            Self::Safe => tools.client.is_empty(),
            Self::Strict => tools.is_empty(),
        }
    }
}

/// 请求里转成别的方言后会丢的工具。入口方言自己的普通函数工具不算。
#[must_use]
pub fn foreign_tools(format: ApiFormat, body: &Value) -> ForeignTools {
    let mut found = ForeignTools::default();
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return found;
    };
    for tool in tools {
        match format {
            ApiFormat::Responses | ApiFormat::ResponsesCompact => {
                match tool.get("type").and_then(Value::as_str) {
                    Some("function") | None => {}
                    Some(kind) => found.push(kind, responses_hosted(kind)),
                }
            }
            ApiFormat::ChatCompletions => match tool.get("type").and_then(Value::as_str) {
                Some("function") | None => {}
                // 与 Responses 臂同一张托管表：同名工具不因入口不同而被分到两边
                Some(kind) => found.push(kind, responses_hosted(kind)),
            },
            ApiFormat::Messages => match tool.get("type").and_then(Value::as_str) {
                Some("custom") | None => {}
                Some(kind) => found.push(kind, anthropic_hosted(kind)),
            },
            // 带 `type` 的不是 Gemini 的 Tool，而是随 Gemini 请求进来的 Anthropic 原生工具
            // （`{"type":"web_search_20250305","name":..,"max_uses":..}`）：按 `type` 认，别把它的
            // 字段名 type / name / max_uses 当成三种工具。
            ApiFormat::Gemini => {
                if let Some(kind) = tool.get("type").and_then(Value::as_str) {
                    found.push(kind, anthropic_hosted(kind));
                    continue;
                }
                // Gemini 的 Tool 以字段名区分种类；值为 null 的字段是 SDK 把未设的可选字段也序列化了，
                // 不是声明了这个工具。
                for (kind, value) in tool.as_object().into_iter().flatten() {
                    if !value.is_null()
                        && !matches!(
                            kind.as_str(),
                            "functionDeclarations" | "function_declarations"
                        )
                    {
                        found.push(kind, gemini_hosted(kind));
                    }
                }
            }
        }
    }
    found
}

/// Responses 内置工具里由上游执行的那些；其余（`custom`、`local_shell`、`computer_use*`……）在客户端执行。
/// Anthropic 的服务端工具（`web_fetch_*`、`code_execution_*`）也能随 Responses / Chat 请求进来，同样是托管的。
fn responses_hosted(kind: &str) -> bool {
    kind.starts_with("web_search")
        || anthropic_hosted(kind)
        || matches!(
            kind,
            "file_search" | "code_interpreter" | "image_generation" | "mcp"
        )
}

/// Anthropic 的服务端工具（搜索、抓取、代码执行）由上游执行；bash / text_editor / computer / memory 在客户端。
fn anthropic_hosted(kind: &str) -> bool {
    ["web_search_", "web_fetch_", "code_execution_"]
        .iter()
        .any(|prefix| kind.starts_with(prefix))
}

/// Gemini 的内置工具除 computer use 外都由上游执行。
fn gemini_hosted(kind: &str) -> bool {
    !matches!(kind, "computerUse" | "computer_use")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn responses_split_hosted_from_client_tools_and_ignore_functions() {
        let body = json!({"tools": [
            {"type": "function", "name": "shell"},
            {"type": "custom", "name": "apply_patch"},
            {"type": "web_search"},
            {"type": "local_shell"},
            {"type": "web_search_preview_2025_03_11"},
            {"type": "custom", "name": "other"},
        ]});
        let found = foreign_tools(ApiFormat::Responses, &body);
        assert_eq!(
            found.hosted,
            ["web_search", "web_search_preview_2025_03_11"]
        );
        assert_eq!(found.client, ["custom", "local_shell"]);
        assert!(
            foreign_tools(
                ApiFormat::Responses,
                &json!({"tools": [{"type": "function", "name": "f"}]})
            )
            .is_empty()
        );
        assert!(foreign_tools(ApiFormat::Responses, &json!({"input": "hi"})).is_empty());
    }

    #[test]
    fn other_dialects_flag_their_own_non_function_tools() {
        let chat = json!({"tools": [{"type": "function", "function": {"name": "f"}}, {"type": "custom", "custom": {"name": "apply_patch"}}]});
        assert_eq!(
            foreign_tools(ApiFormat::ChatCompletions, &chat).client,
            ["custom"]
        );
        let messages = json!({"tools": [
            {"name": "f", "input_schema": {"type": "object"}},
            {"type": "custom", "name": "g", "input_schema": {"type": "object"}},
            {"type": "bash_20250124", "name": "bash"},
            {"type": "web_search_20250305", "name": "web_search"},
        ]});
        let found = foreign_tools(ApiFormat::Messages, &messages);
        assert_eq!(
            (found.hosted, found.client),
            (
                vec!["web_search_20250305".to_owned()],
                vec!["bash_20250124".to_owned()]
            )
        );
        let gemini = json!({"tools": [{"functionDeclarations": []}, {"googleSearch": {}}, {"computerUse": {}}]});
        let found = foreign_tools(ApiFormat::Gemini, &gemini);
        assert_eq!(
            (found.hosted, found.client),
            (
                vec!["googleSearch".to_owned()],
                vec!["computerUse".to_owned()]
            )
        );
    }

    #[test]
    fn anthropic_server_tools_are_hosted_in_any_dialect_and_carried_ones_drop_out() {
        let body = json!({"tools": [
            {"type": "web_fetch_20250910", "name": "web_fetch"},
            {"type": "code_execution_20250825", "name": "code_execution"},
            {"type": "custom", "name": "apply_patch"},
        ]});
        for format in [ApiFormat::Responses, ApiFormat::ChatCompletions] {
            let found = foreign_tools(format, &body);
            assert_eq!(
                found.hosted,
                ["web_fetch_20250910", "code_execution_20250825"]
            );
            assert_eq!(found.client, ["custom"]);
            let left = found.without(&["web_fetch_20250910", "code_execution_20250825"]);
            assert_eq!(
                (left.hosted.len(), left.client.as_slice()),
                (0, ["custom".to_owned()].as_slice())
            );
        }
    }

    #[test]
    fn gemini_reads_tool_kinds_from_set_fields_and_native_tools_from_type() {
        let gemini = json!({"tools": [{"functionDeclarations": [], "googleSearch": null, "computerUse": null}]});
        assert!(foreign_tools(ApiFormat::Gemini, &gemini).is_empty());
        let native = json!({"tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 2}]});
        let found = foreign_tools(ApiFormat::Gemini, &native);
        assert_eq!(
            (found.hosted.as_slice(), found.client.len()),
            (["web_search_20250305".to_owned()].as_slice(), 0)
        );
    }

    #[test]
    fn policies_decide_which_losses_a_channel_accepts() {
        let hosted = ForeignTools {
            hosted: vec!["web_search".into()],
            client: vec![],
        };
        let client = ForeignTools {
            hosted: vec![],
            client: vec!["custom".into()],
        };
        assert!(ToolLossPolicy::parse(None).admits(&hosted));
        assert!(!ToolLossPolicy::parse(None).admits(&client));
        assert!(!ToolLossPolicy::parse(Some("strict")).admits(&hosted));
        assert!(ToolLossPolicy::parse(Some("allow")).admits(&client));
        assert_eq!(ToolLossPolicy::parse(Some("bogus")), ToolLossPolicy::Safe);
    }
}
