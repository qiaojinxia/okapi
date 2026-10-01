//! /v1/chat/completions 的探针 DTO。

use okapi_domain::{ModalitiesReported, TokenUsage};
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
    #[serde(default, deserialize_with = "tool_json")]
    pub tools: String,
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
        self.prompt_segments()
            .iter()
            .map(|s| s.chars().count())
            .sum()
    }

    /// prompt 可见文本片段（分词估算输入）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(self.messages.len());
        for m in &self.messages {
            push_text(&m.content, &mut out);
        }
        if !self.tools.is_empty() {
            out.push(&self.tools);
        }
        out
    }
}

/// 把可见文本片段借出来交给分词器。
/// 返回借用而非拼接的 String——prompt 可以很大，热路径上不该多一次整段拷贝。
fn push_text<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(s) => out.push(s),
        serde_json::Value::Array(parts) => {
            for part in parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    out.push(t);
                }
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Object(_) => {}
    }
}

fn tool_json<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(de)?;
    Ok(
        if value.is_null() || value.as_array().is_some_and(Vec::is_empty) {
            String::new()
        } else {
            value.to_string()
        },
    )
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
    #[serde(default, deserialize_with = "tool_json")]
    pub tools: String,
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
        self.prompt_segments()
            .iter()
            .map(|s| s.chars().count())
            .sum()
    }

    /// prompt 可见文本片段（含顶层 system）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        push_text(&self.system, &mut out);
        for m in &self.messages {
            push_text(&m.content, &mut out);
        }
        if !self.tools.is_empty() {
            out.push(&self.tools);
        }
        out
    }
}

/// OpenAI Responses API 请求探针（入口解析用；直转与降级共用）。
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
    /// Responses 与 chat 同样接受 service_tier（tier 计费轴输入；直转时随体透传）。
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default, deserialize_with = "tool_json")]
    pub tools: String,
}

impl ResponsesRequestProbe {
    #[must_use]
    pub fn completion_cap_req(&self) -> Option<u32> {
        self.max_output_tokens
    }

    /// prompt 可见文本总字符数（instructions + input）。
    #[must_use]
    pub fn prompt_chars(&self) -> usize {
        self.prompt_segments()
            .iter()
            .map(|s| s.chars().count())
            .sum()
    }

    /// prompt 可见文本片段（instructions + input，含 input 项内嵌 content）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<&str> {
        let mut out = Vec::new();
        if let Some(s) = self.instructions.as_deref() {
            out.push(s);
        }
        push_text(&self.input, &mut out);
        if let serde_json::Value::Array(items) = &self.input {
            for item in items {
                if let Some(c) = item.get("content") {
                    push_text(c, &mut out);
                }
            }
        }
        if !self.tools.is_empty() {
            out.push(&self.tools);
        }
        out
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

/// Gemini `models/{model}:generateContent` 请求探针（原生入口解析用）。
/// 模型名与流式与否都在 URL 上，不在 body；这里只解析估算与粘性需要的最小集。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GeminiRequestProbe {
    #[serde(default)]
    pub contents: Vec<GeminiContentProbe>,
    /// `systemInstruction`（camelCase）与 `system_instruction`（snake_case）官方都收。
    #[serde(default, alias = "system_instruction")]
    pub system_instruction: Option<GeminiContentProbe>,
    #[serde(default, alias = "generation_config")]
    pub generation_config: Option<GeminiGenerationConfigProbe>,
    #[serde(default, deserialize_with = "tool_json")]
    pub tools: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GeminiContentProbe {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GeminiGenerationConfigProbe {
    #[serde(default, alias = "max_output_tokens")]
    pub max_output_tokens: Option<u32>,
}

impl GeminiRequestProbe {
    #[must_use]
    pub fn completion_cap_req(&self) -> Option<u32> {
        self.generation_config
            .as_ref()
            .and_then(|c| c.max_output_tokens)
    }

    /// prompt 可见文本总字符数（systemInstruction + contents 的 text 部件）。
    #[must_use]
    pub fn prompt_chars(&self) -> usize {
        self.prompt_segments()
            .iter()
            .map(|s| s.chars().count())
            .sum()
    }

    /// prompt 可见文本片段（systemInstruction 在前）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for c in self.system_instruction.iter().chain(self.contents.iter()) {
            for part in &c.parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    out.push(t);
                }
            }
        }
        if !self.tools.is_empty() {
            out.push(&self.tools);
        }
        out
    }

    /// contents → 消息探针（会话粘性种子用）：Gemini 的 `model` 角色映射为 assistant，
    /// text 部件按 OpenAI 多段形状放进 content，粘性哈希对两种入口一致。
    #[must_use]
    pub fn input_messages(&self) -> Vec<MessageProbe> {
        self.contents
            .iter()
            .map(|c| MessageProbe {
                role: match c.role.as_deref() {
                    Some("model") => "assistant".to_owned(),
                    Some(r) if !r.is_empty() => r.to_owned(),
                    _ => "user".to_owned(),
                },
                content: serde_json::Value::Array(
                    c.parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .map(|t| serde_json::json!({"type": "text", "text": t}))
                        .collect(),
                ),
            })
            .collect()
    }
}

/// OpenAI usage 探针；缺失轴由标记保留，不能把占位零当作实报。
#[derive(Debug, Clone, Copy, Default)]
pub struct UsageProbe {
    pub missing_prompt: bool,
    pub missing_completion: bool,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub prompt_tokens_details: PromptTokensDetails,
    pub completion_tokens_details: CompletionTokensDetails,
    /// Internal parse failure. Kept distinct from absent usage so settlement cannot estimate it.
    pub invalid: bool,
}

/// 字段名与 OpenAI 官方 `prompt_tokens_details` 一致（openai-python
/// `completion_usage.py`），故 OpenAI 系响应可直接反序列化。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(from = "RawPromptTokensDetails")]
pub struct PromptTokensDetails {
    pub cached_tokens: u32,
    /// 兼容扩展：缓存写入；Anthropic 由 cache_creation_input_tokens 映射。
    pub cache_write_tokens: u32,
    pub cache_write_5m_tokens: Option<u32>,
    pub cache_write_1h_tokens: Option<u32>,
    pub cache_read_reported: bool,
    pub cache_write_reported: bool,
    pub modalities_reported: ModalitiesReported,
    /// 音频输入总量，含其中的缓存部分；价格来自当前模型配置。
    #[serde(default)]
    pub audio_tokens: u32,
    /// 图片输入总量，含其中的缓存部分。
    #[serde(default)]
    pub image_tokens: u32,
    /// Compatible extensions: modal subsets of the cache totals, not extra tokens.
    pub cached_tokens_details: Option<ModalTokensDetails>,
    pub cache_write_tokens_details: Option<ModalTokensDetails>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, serde::Serialize)]
pub struct ModalTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_tokens: Option<u32>,
}

impl ModalTokensDetails {
    /// Explicit fields or complete coverage establish missing subsets as zero.
    #[must_use]
    pub fn reported(self, total: u32) -> ModalitiesReported {
        let covered = u64::from(self.text_tokens.unwrap_or(0))
            + u64::from(self.audio_tokens.unwrap_or(0))
            + u64::from(self.image_tokens.unwrap_or(0))
            == u64::from(total);
        ModalitiesReported {
            audio: self.audio_tokens.is_some() || covered,
            image: self.image_tokens.is_some() || covered,
        }
    }
}

#[derive(Default, Deserialize)]
struct RawPromptTokensDetails {
    #[serde(rename = "cached_tokens")]
    cached: Option<u32>,
    #[serde(rename = "cache_write_tokens")]
    cache_write: Option<u32>,
    cache_write_5m_tokens: Option<u32>,
    cache_write_1h_tokens: Option<u32>,
    #[serde(rename = "audio_tokens")]
    audio: Option<u32>,
    #[serde(rename = "image_tokens")]
    image: Option<u32>,
    cached_tokens_details: Option<ModalTokensDetails>,
    cache_write_tokens_details: Option<ModalTokensDetails>,
}

impl From<RawPromptTokensDetails> for PromptTokensDetails {
    fn from(raw: RawPromptTokensDetails) -> Self {
        Self {
            cached_tokens: raw.cached.unwrap_or(0),
            cache_write_tokens: raw.cache_write.unwrap_or(0),
            cache_write_5m_tokens: raw.cache_write_5m_tokens,
            cache_write_1h_tokens: raw.cache_write_1h_tokens,
            cache_read_reported: raw.cached.is_some(),
            cache_write_reported: raw.cache_write.is_some(),
            audio_tokens: raw.audio.unwrap_or(0),
            image_tokens: raw.image.unwrap_or(0),
            modalities_reported: ModalitiesReported {
                audio: raw.audio.is_some(),
                image: raw.image.is_some(),
            },
            cached_tokens_details: raw.cached_tokens_details,
            cache_write_tokens_details: raw.cache_write_tokens_details,
        }
    }
}

impl PromptTokensDetails {
    fn with_previous(mut self, previous: Self) -> Self {
        if !self.cache_read_reported && self.cached_tokens == 0 {
            self.cached_tokens = previous.cached_tokens;
            self.cache_read_reported = previous.cache_read_reported;
            self.cached_tokens_details = self
                .cached_tokens_details
                .or(previous.cached_tokens_details);
        }
        if !self.cache_write_reported && self.cache_write_tokens == 0 {
            self.cache_write_tokens = previous.cache_write_tokens;
            self.cache_write_reported = previous.cache_write_reported;
            if self.cache_write_5m_tokens.is_none() && self.cache_write_1h_tokens.is_none() {
                self.cache_write_5m_tokens = previous.cache_write_5m_tokens;
                self.cache_write_1h_tokens = previous.cache_write_1h_tokens;
            }
            self.cache_write_tokens_details = self
                .cache_write_tokens_details
                .or(previous.cache_write_tokens_details);
        }
        if !self.modalities_reported.audio && self.audio_tokens == 0 {
            self.audio_tokens = previous.audio_tokens;
            self.modalities_reported.audio = previous.modalities_reported.audio;
        }
        if !self.modalities_reported.image && self.image_tokens == 0 {
            self.image_tokens = previous.image_tokens;
            self.modalities_reported.image = previous.modalities_reported.image;
        }
        self
    }

    /// 协议转换时保留缺失状态，避免下游把补出的 0 当成明确上报。
    #[must_use]
    pub fn cache_json(self) -> serde_json::Value {
        let mut value = serde_json::json!({});
        if self.cache_read_reported || self.cached_tokens > 0 {
            value["cached_tokens"] = serde_json::json!(self.cached_tokens);
        }
        if self.cache_write_reported || self.cache_write_tokens > 0 {
            value["cache_write_tokens"] = serde_json::json!(self.cache_write_tokens);
        }
        if let (Some(short), Some(long)) = (self.cache_write_5m_tokens, self.cache_write_1h_tokens)
        {
            value["cache_write_5m_tokens"] = serde_json::json!(short);
            value["cache_write_1h_tokens"] = serde_json::json!(long);
        }
        if self.modalities_reported.audio || self.audio_tokens > 0 {
            value["audio_tokens"] = serde_json::json!(self.audio_tokens);
        }
        if self.modalities_reported.image || self.image_tokens > 0 {
            value["image_tokens"] = serde_json::json!(self.image_tokens);
        }
        if let Some(details) = self.cached_tokens_details {
            value["cached_tokens_details"] = serde_json::json!(details);
        }
        if let Some(details) = self.cache_write_tokens_details {
            value["cache_write_tokens_details"] = serde_json::json!(details);
        }
        value
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(from = "RawCompletionTokensDetails")]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
    /// 音频输出 token（官方字段同名）。
    #[serde(default)]
    pub audio_tokens: u32,
    /// Compatible extension used by native image-producing adapters.
    #[serde(default)]
    pub image_tokens: u32,
    pub modalities_reported: ModalitiesReported,
    pub reasoning_reported: bool,
}

#[derive(Default, Deserialize)]
struct RawCompletionTokensDetails {
    #[serde(rename = "reasoning_tokens")]
    reasoning: Option<u32>,
    #[serde(rename = "audio_tokens")]
    audio: Option<u32>,
    #[serde(rename = "image_tokens")]
    image: Option<u32>,
}

impl From<RawCompletionTokensDetails> for CompletionTokensDetails {
    fn from(raw: RawCompletionTokensDetails) -> Self {
        Self {
            reasoning_tokens: raw.reasoning.unwrap_or(0),
            audio_tokens: raw.audio.unwrap_or(0),
            image_tokens: raw.image.unwrap_or(0),
            modalities_reported: ModalitiesReported {
                audio: raw.audio.is_some(),
                image: raw.image.is_some(),
            },
            reasoning_reported: raw.reasoning.is_some(),
        }
    }
}

impl UsageProbe {
    /// Cumulative stream snapshots replace counts, never add them. A missing axis
    /// retains the previous observation; explicit zero replaces it. Invalid data
    /// poisons the sequence even if a later event looks valid.
    #[must_use]
    pub fn with_previous(mut self, previous: Option<Self>) -> Self {
        let Some(previous) = previous else {
            return self;
        };
        if self.invalid || previous.with_estimates(0, 0).is_err() {
            return Self::invalid();
        }
        if self.missing_prompt && !previous.missing_prompt {
            self.prompt_tokens = previous.prompt_tokens;
            self.missing_prompt = false;
        }
        if self.missing_completion && !previous.missing_completion {
            self.completion_tokens = previous.completion_tokens;
            self.missing_completion = false;
        }
        self.prompt_tokens_details = self
            .prompt_tokens_details
            .with_previous(previous.prompt_tokens_details);
        self.completion_tokens_details = self
            .completion_tokens_details
            .with_previous(previous.completion_tokens_details);
        if self.with_estimates(0, 0).is_err() {
            return Self::invalid();
        }
        self
    }

    #[must_use]
    pub fn invalid() -> Self {
        Self {
            invalid: true,
            ..Self::default()
        }
    }
    /// Normalize intersecting cache/modality counts without clipping or guessing.
    /// Invalid or ambiguous counts must not become a valid-looking bill.
    pub fn to_token_usage(self) -> Result<TokenUsage, okapi_domain::DomainError> {
        super::token_usage::normalize(self)
    }

    /// Fill only absent axes, retaining original counts for audit and local overrides.
    pub fn with_estimates(
        self,
        prompt: u32,
        completion: u32,
    ) -> Result<TokenUsage, okapi_domain::DomainError> {
        super::token_usage::with_estimates(self, prompt, completion)
    }

    /// Conversion JSON must preserve absence, including across a second protocol hop.
    #[must_use]
    pub fn chat_json(self) -> serde_json::Value {
        if self.with_estimates(0, 0).is_err() {
            return serde_json::Value::Null;
        }
        let mut value = serde_json::json!({
            "prompt_tokens_details": self.prompt_tokens_details.cache_json(),
            "completion_tokens_details": self.completion_tokens_details.to_json(),
        });
        if !self.missing_prompt {
            value["prompt_tokens"] = self.prompt_tokens.into();
        }
        if !self.missing_completion {
            value["completion_tokens"] = self.completion_tokens.into();
        }
        if !self.missing_prompt && !self.missing_completion {
            value["total_tokens"] =
                (u64::from(self.prompt_tokens) + u64::from(self.completion_tokens)).into();
        }
        value
    }
}

impl CompletionTokensDetails {
    fn with_previous(mut self, previous: Self) -> Self {
        if !self.reasoning_reported && self.reasoning_tokens == 0 {
            self.reasoning_tokens = previous.reasoning_tokens;
            self.reasoning_reported = previous.reasoning_reported;
        }
        if !self.modalities_reported.audio && self.audio_tokens == 0 {
            self.audio_tokens = previous.audio_tokens;
            self.modalities_reported.audio = previous.modalities_reported.audio;
        }
        if !self.modalities_reported.image && self.image_tokens == 0 {
            self.image_tokens = previous.image_tokens;
            self.modalities_reported.image = previous.modalities_reported.image;
        }
        self
    }

    #[must_use]
    pub fn to_json(self) -> serde_json::Value {
        let mut value = serde_json::json!({});
        if self.reasoning_reported || self.reasoning_tokens > 0 {
            value["reasoning_tokens"] = serde_json::json!(self.reasoning_tokens);
        }
        if self.modalities_reported.audio || self.audio_tokens > 0 {
            value["audio_tokens"] = serde_json::json!(self.audio_tokens);
        }
        if self.modalities_reported.image || self.image_tokens > 0 {
            value["image_tokens"] = serde_json::json!(self.image_tokens);
        }
        value
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
    #[serde(default, alias = "reasoning")]
    pub reasoning_content: Option<String>,
}

impl ChunkProbe {
    /// 是否携带实际产出（内容/工具调用/拒答文本）——首字判定与空回复判定共用。
    #[must_use]
    pub fn has_output(&self) -> bool {
        self.choices.iter().any(|c| {
            c.delta.content.as_ref().is_some_and(|s| !s.is_empty())
                || c.delta.tool_calls.is_some()
                || c.delta
                    .reasoning_content
                    .as_ref()
                    .is_some_and(|s| !s.is_empty())
                || c.delta.refusal.as_ref().is_some_and(|s| !s.is_empty())
        })
    }

    /// 本 chunk 的可见内容字符数（无 usage 时的补全估算输入）。
    #[must_use]
    pub fn content_chars(&self) -> usize {
        self.choices
            .iter()
            .flat_map(|c| {
                [
                    c.delta.content.as_deref(),
                    c.delta.reasoning_content.as_deref(),
                    c.delta.refusal.as_deref(),
                ]
                .into_iter()
                .flatten()
            })
            .map(|s| s.chars().count())
            .sum()
    }
}

#[cfg(test)]
mod third_review_tests {
    use super::*;
    #[test]
    fn tools_contribute_identical_json_to_all_dialect_probes() {
        let tools = serde_json::json!([{"type":"function","function":{"name":"search","parameters":{"type":"object","properties":{"query":{"type":"string"}},"maxItems":100}}}]);
        let body = serde_json::json!({"model":"fixture","tools":tools});
        let chat: ChatRequestProbe = serde_json::from_value(body.clone()).unwrap();
        let messages: MessagesRequestProbe = serde_json::from_value(body.clone()).unwrap();
        let responses: ResponsesRequestProbe = serde_json::from_value(body.clone()).unwrap();
        let gemini: GeminiRequestProbe = serde_json::from_value(body).unwrap();
        assert_eq!(chat.prompt_segments(), vec![tools.to_string().as_str()]);
        assert_eq!(messages.prompt_chars(), chat.prompt_chars());
        assert_eq!(responses.prompt_chars(), chat.prompt_chars());
        assert_eq!(gemini.prompt_chars(), chat.prompt_chars());
    }
    #[test]
    fn reasoning_only_chunks_are_billable_output() {
        let chunk: ChunkProbe =
            serde_json::from_str(r#"{"choices":[{"delta":{"reasoning_content":"thinking"}}]}"#)
                .unwrap();
        assert!(chunk.has_output());
        assert_eq!(chunk.content_chars(), 8);
    }
}
