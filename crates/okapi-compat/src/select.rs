//! 选择器的编译形态与匹配。

use crate::glob;
use crate::rule::Selector;
use serde::{Deserialize, Serialize};

/// 一次请求的上下文：规则能看到、选择器能匹配的全部事实。引擎不读任何别的东西。
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// 渠道的厂商（带命名空间，如 `builtin:deepseek`）；没选厂商为 `None`。
    pub vendor: Option<&'a str>,
    /// 渠道 `api_base` 的主机名。
    pub host: Option<&'a str>,
    /// 映射后的上游模型名。
    pub model: &'a str,
    /// 客户端请求的模型名。
    pub client_model: &'a str,
    /// 上游方言（见 [`DIALECTS`]）。
    pub dialect: &'a str,
    /// 入口操作（见 [`OPERATIONS`]）。
    pub operation: &'a str,
    pub stream: bool,
    /// 入口客户端类别；识别不出为 `None`。
    pub client: Option<&'a str>,
}

/// 管理 API 收发的拥有型版本（dry-run 的请求体用）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextInput {
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub client_model: Option<String>,
    #[serde(default = "default_dialect")]
    pub dialect: String,
    #[serde(default = "default_operation")]
    pub operation: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub client: Option<String>,
}

fn default_dialect() -> String {
    "openai.chat".to_owned()
}

fn default_operation() -> String {
    "chat".to_owned()
}

impl ContextInput {
    #[must_use]
    pub fn as_context(&self) -> Context<'_> {
        Context {
            vendor: self.vendor.as_deref(),
            host: self.host.as_deref(),
            model: &self.model,
            client_model: self.client_model.as_deref().unwrap_or(&self.model),
            dialect: &self.dialect,
            operation: &self.operation,
            stream: self.stream,
            client: self.client.as_deref(),
        }
    }
}

pub const DIALECTS: [&str; 4] = [
    "openai.chat",
    "openai.responses",
    "anthropic.messages",
    "gemini.generate",
];

pub const OPERATIONS: [&str; 6] = [
    "chat",
    "responses",
    "embeddings",
    "images",
    "rerank",
    "audio",
];

/// 裸 id 视为内置厂商：`deepseek` ≡ `builtin:deepseek`。
#[must_use]
pub fn normalize_vendor(id: &str) -> String {
    let id = id.to_ascii_lowercase();
    if id.contains(':') {
        id
    } else {
        format!("builtin:{id}")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CompiledSelector {
    vendor: Vec<String>,
    host: Vec<String>,
    model: Vec<String>,
    client_model: Vec<String>,
    dialect: Vec<String>,
    operation: Vec<String>,
    stream: Option<bool>,
    client: Vec<String>,
}

fn lower(items: &[String]) -> Vec<String> {
    items.iter().map(|s| s.to_ascii_lowercase()).collect()
}

impl CompiledSelector {
    pub(crate) fn compile(select: &Selector) -> Self {
        Self {
            vendor: select.vendor.iter().map(|v| normalize_vendor(v)).collect(),
            host: lower(&select.host),
            model: lower(&select.model),
            client_model: lower(&select.client_model),
            dialect: select.dialect.clone(),
            operation: select.operation.clone(),
            stream: select.stream,
            client: lower(&select.client),
        }
    }

    pub(crate) fn matches(&self, ctx: &Context<'_>) -> bool {
        let any_glob =
            |patterns: &[String], text: &str| patterns.iter().any(|p| glob::matches(p, text));
        let any_eq = |items: &[String], text: &str| items.iter().any(|i| i == text);
        if let Some(stream) = self.stream
            && stream != ctx.stream
        {
            return false;
        }
        if !self.vendor.is_empty() {
            let Some(vendor) = ctx.vendor else {
                return false;
            };
            if !any_eq(&self.vendor, &normalize_vendor(vendor)) {
                return false;
            }
        }
        if !self.host.is_empty() {
            let Some(host) = ctx.host else {
                return false;
            };
            if !any_glob(&self.host, host) {
                return false;
            }
        }
        if !self.client.is_empty() {
            let Some(client) = ctx.client else {
                return false;
            };
            if !any_eq(&self.client, &client.to_ascii_lowercase()) {
                return false;
            }
        }
        (self.model.is_empty() || any_glob(&self.model, ctx.model))
            && (self.client_model.is_empty() || any_glob(&self.client_model, ctx.client_model))
            && (self.dialect.is_empty() || any_eq(&self.dialect, ctx.dialect))
            && (self.operation.is_empty() || any_eq(&self.operation, ctx.operation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context<'static> {
        Context {
            vendor: Some("builtin:moonshot"),
            host: Some("api.moonshot.cn"),
            model: "kimi-k2.6",
            client_model: "kimi",
            dialect: "openai.chat",
            operation: "chat",
            stream: true,
            client: Some("claude-code"),
        }
    }

    fn sel(json: serde_json::Value) -> CompiledSelector {
        CompiledSelector::compile(&serde_json::from_value(json).expect("selector"))
    }

    #[test]
    fn empty_selector_matches_everything() {
        assert!(CompiledSelector::default().matches(&ctx()));
    }

    #[test]
    fn every_field_is_anded() {
        use serde_json::json;
        assert!(
            sel(json!({"vendor": ["moonshot"], "model": ["kimi-*"], "stream": true}))
                .matches(&ctx())
        );
        assert!(
            !sel(json!({"vendor": ["moonshot"], "model": ["kimi-*"], "stream": false}))
                .matches(&ctx())
        );
        assert!(!sel(json!({"vendor": ["deepseek"], "model": ["kimi-*"]})).matches(&ctx()));
        assert!(sel(json!({"host": ["*.moonshot.cn"]})).matches(&ctx()));
        assert!(sel(json!({"dialect": ["openai.chat", "openai.responses"]})).matches(&ctx()));
        assert!(!sel(json!({"dialect": ["anthropic.messages"]})).matches(&ctx()));
        assert!(sel(json!({"client": ["Claude-Code"]})).matches(&ctx()));
        assert!(sel(json!({"client_model": ["kimi"]})).matches(&ctx()));
        assert!(sel(json!({"operation": ["chat"]})).matches(&ctx()));
    }

    #[test]
    fn vendor_namespaces_normalize() {
        use serde_json::json;
        assert!(sel(json!({"vendor": ["builtin:moonshot"]})).matches(&ctx()));
        assert!(!sel(json!({"vendor": ["custom:moonshot"]})).matches(&ctx()));
        let mut bare = ctx();
        bare.vendor = Some("moonshot");
        assert!(sel(json!({"vendor": ["moonshot"]})).matches(&bare));
    }

    #[test]
    fn missing_context_facts_never_match_a_constrained_selector() {
        use serde_json::json;
        let mut bare = ctx();
        bare.vendor = None;
        bare.host = None;
        bare.client = None;
        assert!(!sel(json!({"vendor": ["moonshot"]})).matches(&bare));
        assert!(!sel(json!({"host": ["*"]})).matches(&bare));
        assert!(!sel(json!({"client": ["claude-code"]})).matches(&bare));
        assert!(sel(json!({"model": ["kimi-k2.6"]})).matches(&bare));
    }

    #[test]
    fn input_context_defaults_client_model_to_model() {
        let input: ContextInput =
            serde_json::from_value(serde_json::json!({"model": "m"})).expect("input");
        let ctx = input.as_context();
        assert_eq!(ctx.client_model, "m");
        assert_eq!(ctx.dialect, "openai.chat");
        assert_eq!(ctx.operation, "chat");
        assert!(!ctx.stream);
    }
}
