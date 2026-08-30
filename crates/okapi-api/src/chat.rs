//! /v1/chat/completions 的探针 DTO。

use okapi_domain::TokenUsage;
use serde::Deserialize;

/// 请求探针：解析失败即 400；未知字段全部保留在原始 body 中透传。
#[derive(Debug, Clone, Deserialize)]
pub struct ChatRequestProbe {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    /// OpenAI service_tier（auto/default/flex/priority；tier 计费轴输入）。
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub messages: Vec<MessageProbe>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageProbe {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: serde_json::Value,
}

impl ChatRequestProbe {
    /// 预扣用的补全上限：max_completion_tokens > max_tokens > 模型缺省。
    #[must_use]
    pub fn completion_cap(&self, model_default: u32) -> u32 {
        self.max_completion_tokens
            .or(self.max_tokens)
            .unwrap_or(model_default)
    }

    /// prompt 可见文本总字符数（估算输入）。
    #[must_use]
    pub fn prompt_chars(&self) -> usize {
        self.messages
            .iter()
            .map(|m| content_chars(&m.content))
            .sum()
    }
}

fn content_chars(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(s) => s.chars().count(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .map(|p| {
                p.get("text")
                    .and_then(|t| t.as_str())
                    .map_or(0, |s| s.chars().count())
            })
            .sum(),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Object(_) => 0,
    }
}

/// Anthropic /v1/messages 请求探针（入口协议解析用，字段最小集）。
#[derive(Debug, Clone, Deserialize)]
pub struct MessagesRequestProbe {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub messages: Vec<MessageProbe>,
    #[serde(default)]
    pub system: serde_json::Value,
}

impl MessagesRequestProbe {
    /// 预扣用的补全上限：max_tokens > 模型缺省。
    #[must_use]
    pub fn completion_cap(&self, model_default: u32) -> u32 {
        self.max_tokens.unwrap_or(model_default)
    }

    /// prompt 可见文本总字符数（含顶层 system）。
    #[must_use]
    pub fn prompt_chars(&self) -> usize {
        let msg_chars: usize = self
            .messages
            .iter()
            .map(|m| content_chars(&m.content))
            .sum();
        msg_chars + content_chars(&self.system)
    }
}

/// OpenAI Responses API 请求探针（降级入口解析用）。
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequestProbe {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub input: serde_json::Value,
    #[serde(default)]
    pub instructions: Option<String>,
}

impl ResponsesRequestProbe {
    #[must_use]
    pub fn completion_cap_req(&self) -> Option<u32> {
        self.max_output_tokens
    }

    /// prompt 可见文本总字符数（instructions + input）。
    #[must_use]
    pub fn prompt_chars(&self) -> usize {
        let base = self
            .instructions
            .as_deref()
            .map_or(0, |s| s.chars().count());
        base + content_chars(&self.input)
            + match &self.input {
                serde_json::Value::Array(items) => items
                    .iter()
                    .map(|i| content_chars(i.get("content").unwrap_or(&serde_json::Value::Null)))
                    .sum(),
                _ => 0,
            }
    }

    /// input → 消息探针（会话粘性种子用；string 视为单条 user）。
    #[must_use]
    pub fn input_messages(&self) -> Vec<MessageProbe> {
        match &self.input {
            serde_json::Value::String(s) => vec![MessageProbe {
                role: "user".to_owned(),
                content: serde_json::Value::String(s.clone()),
            }],
            serde_json::Value::Array(items) => items
                .iter()
                .map(|i| MessageProbe {
                    role: i
                        .get("role")
                        .and_then(|r| r.as_str())
                        .unwrap_or("user")
                        .to_owned(),
                    content: i.get("content").cloned().unwrap_or(serde_json::Value::Null),
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// OpenAI usage 探针（含缓存/推理细分；上游缺字段时取 0）。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct UsageProbe {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub prompt_tokens_details: PromptTokensDetails,
    #[serde(default)]
    pub completion_tokens_details: CompletionTokensDetails,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
    /// 缓存**写入** token。OpenAI 口径无此字段（隐式缓存不单独计价），
    /// 由 Anthropic 方向的 `cache_creation_input_tokens` 填入。
    #[serde(default)]
    pub cache_write_tokens: u32,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
}

impl UsageProbe {
    /// 转领域用量。
    ///
    /// 脏数据收敛（fail-safe 而非 fail-closed：上游 usage 不可信，但不能因此拒绝已完成的请求）：
    /// cached 先收敛到 prompt，cache_write 再收敛到剩余额度，保证
    /// `cached + cache_write <= prompt` 恒成立、常规输入段不被截断。
    #[must_use]
    pub fn to_token_usage(self) -> TokenUsage {
        let cached = self
            .prompt_tokens_details
            .cached_tokens
            .min(self.prompt_tokens);
        let cache_write = self
            .prompt_tokens_details
            .cache_write_tokens
            .min(self.prompt_tokens - cached);
        TokenUsage {
            prompt_tokens: self.prompt_tokens,
            cached_tokens: cached,
            cache_write_tokens: cache_write,
            completion_tokens: self.completion_tokens,
            reasoning_tokens: self
                .completion_tokens_details
                .reasoning_tokens
                .min(self.completion_tokens),
        }
    }
}

/// 流式 chunk 探针：识别首个内容事件与随流 usage。
#[derive(Debug, Clone, Deserialize)]
pub struct ChunkProbe {
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<UsageProbe>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub delta: ChunkDelta,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChunkDelta {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(default)]
    pub refusal: Option<String>,
}

impl ChunkProbe {
    /// 是否携带实际产出（内容/工具调用/拒答文本）——首字判定与空回复判定共用。
    #[must_use]
    pub fn has_output(&self) -> bool {
        self.choices.iter().any(|c| {
            c.delta.content.as_ref().is_some_and(|s| !s.is_empty())
                || c.delta.tool_calls.is_some()
                || c.delta.refusal.as_ref().is_some_and(|s| !s.is_empty())
        })
    }

    /// 本 chunk 的可见内容字符数（无 usage 时的补全估算输入）。
    #[must_use]
    pub fn content_chars(&self) -> usize {
        self.choices
            .iter()
            .filter_map(|c| c.delta.content.as_deref())
            .map(|s| s.chars().count())
            .sum()
    }
}
