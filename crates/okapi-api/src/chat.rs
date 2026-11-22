//! /v1/chat/completions 的探针 DTO。

use serde::Deserialize;
use std::borrow::Cow;

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
    /// 一次生成的候选条数：补全按条计费，预扣也要乘它。
    #[serde(default)]
    pub n: Option<u32>,
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
    /// OpenAI assistant 消息里此前的工具调用（参数是下一轮的输入）。
    #[serde(default)]
    pub tool_calls: serde_json::Value,
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

    /// 预扣按条数估补全：缺省 1，显式 0 也按 1（上游会拒绝，不该因此少扣）。
    #[must_use]
    pub fn choices(&self) -> u32 {
        self.n.unwrap_or(1).max(1)
    }

    /// prompt 可见文本片段（分词估算输入）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<Cow<'_, str>> {
        let mut out = Vec::with_capacity(self.messages.len());
        for m in &self.messages {
            push_text(&m.content, &mut out);
            for call in m.tool_calls.as_array().into_iter().flatten() {
                push_json(call.pointer("/function/arguments"), &mut out);
            }
        }
        if !self.tools.is_empty() {
            out.push(Cow::Borrowed(&self.tools));
        }
        out
    }
}

/// 把可见文本片段交给分词器：文本借用，只有结构化的工具参数才序列化成新串。
/// prompt 可以很大，热路径上不该为纯文本多一次整段拷贝。
fn push_text<'a>(value: &'a serde_json::Value, out: &mut Vec<Cow<'a, str>>) {
    match value {
        serde_json::Value::String(s) => out.push(Cow::Borrowed(s)),
        serde_json::Value::Array(parts) => {
            for part in parts {
                push_part(part, out);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Object(_) => {}
    }
}

/// 一个内容块。工具往返也是输入：智能体的上下文大半是工具结果（文件内容、命令输出）
/// 和此前的调用参数，只数文本块会把这类请求的预扣与兜底计费低估一个数量级。
fn push_part<'a>(part: &'a serde_json::Value, out: &mut Vec<Cow<'a, str>>) {
    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
        out.push(Cow::Borrowed(text));
        return;
    }
    match part.get("type").and_then(serde_json::Value::as_str) {
        Some("tool_result") => {
            if let Some(content) = part.get("content") {
                push_text(content, out);
            }
        }
        Some("tool_use" | "server_tool_use") => push_json(part.get("input"), out),
        _ => {}
    }
}

/// 工具参数：字符串原样借用，结构化值按 JSON 文本计。
fn push_json<'a>(value: Option<&'a serde_json::Value>, out: &mut Vec<Cow<'a, str>>) {
    match value {
        Some(serde_json::Value::String(s)) => out.push(Cow::Borrowed(s)),
        Some(serde_json::Value::Null) | None => {}
        Some(value) => out.push(Cow::Owned(value.to_string())),
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
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl MessagesRequestProbe {
    /// Claude Code 在 `metadata.user_id` 里带的会话 ID：新版是 JSON 字符串
    /// `{"device_id","account_uuid","session_id"}`，旧版是 `user_<hash>_account_<uuid>_session_<uuid>`。
    #[must_use]
    pub fn client_session_id(&self) -> Option<String> {
        let user_id = self.metadata.get("user_id")?.as_str()?;
        let session = if user_id.starts_with('{') {
            serde_json::from_str::<serde_json::Value>(user_id)
                .ok()?
                .get("session_id")?
                .as_str()?
                .to_owned()
        } else {
            user_id.rsplit_once("_session_")?.1.to_owned()
        };
        let session = session.trim();
        (!session.is_empty()).then(|| session.to_owned())
    }

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

    /// prompt 可见文本片段（含顶层 system；tool_use / tool_result 块由 `push_part` 计入）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<Cow<'_, str>> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        push_text(&self.system, &mut out);
        for m in &self.messages {
            push_text(&m.content, &mut out);
        }
        if !self.tools.is_empty() {
            out.push(Cow::Borrowed(&self.tools));
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
    /// 后台模式：上游先回「排队中」、不带用量，生成在请求结束后继续。
    #[serde(default)]
    pub background: Option<bool>,
    #[serde(default, deserialize_with = "tool_json")]
    pub tools: String,
    /// 客户端声明的缓存分组（Codex CLI 填会话 id），作 L2 粘性键。
    #[serde(default)]
    pub prompt_cache_key: Option<String>,
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

    /// prompt 可见文本片段（instructions + input，含 input 项内嵌 content 与工具往返）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<Cow<'_, str>> {
        let mut out = Vec::new();
        if let Some(s) = self.instructions.as_deref() {
            out.push(Cow::Borrowed(s));
        }
        push_text(&self.input, &mut out);
        if let serde_json::Value::Array(items) = &self.input {
            for item in items {
                if let Some(c) = item.get("content") {
                    push_text(c, &mut out);
                }
                match item.get("type").and_then(serde_json::Value::as_str) {
                    Some("function_call") => push_json(item.get("arguments"), &mut out),
                    Some("custom_tool_call") => push_json(item.get("input"), &mut out),
                    Some("function_call_output" | "custom_tool_call_output") => {
                        if let Some(output) = item.get("output") {
                            push_text(output, &mut out);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !self.tools.is_empty() {
            out.push(Cow::Borrowed(&self.tools));
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
                tool_calls: serde_json::Value::Null,
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
                    tool_calls: serde_json::Value::Null,
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
    #[serde(default, rename = "systemInstruction", alias = "system_instruction")]
    pub system_instruction: Option<GeminiContentProbe>,
    #[serde(default, rename = "generationConfig", alias = "generation_config")]
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
    #[serde(default, rename = "maxOutputTokens", alias = "max_output_tokens")]
    pub max_output_tokens: Option<u32>,
    /// 候选条数：每条都按补全计费。
    #[serde(default, rename = "candidateCount", alias = "candidate_count")]
    pub candidate_count: Option<u32>,
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

    /// 预扣按候选条数估补全（缺省 1）。
    #[must_use]
    pub fn choices(&self) -> u32 {
        self.generation_config
            .as_ref()
            .and_then(|c| c.candidate_count)
            .unwrap_or(1)
            .max(1)
    }

    /// prompt 可见文本片段（systemInstruction 在前；functionCall / functionResponse 计入）。
    #[must_use]
    pub fn prompt_segments(&self) -> Vec<Cow<'_, str>> {
        let mut out = Vec::new();
        for c in self.system_instruction.iter().chain(self.contents.iter()) {
            for part in &c.parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    out.push(Cow::Borrowed(t));
                }
                // REST 的 camelCase 与 proto 的 snake_case 官方都收
                push_json(
                    part.pointer("/functionCall/args")
                        .or_else(|| part.pointer("/function_call/args")),
                    &mut out,
                );
                push_json(
                    part.pointer("/functionResponse/response")
                        .or_else(|| part.pointer("/function_response/response")),
                    &mut out,
                );
            }
        }
        if !self.tools.is_empty() {
            out.push(Cow::Borrowed(&self.tools));
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
                tool_calls: serde_json::Value::Null,
            })
            .collect()
    }
}

mod usage;
pub(crate) use usage::cache_counter;
pub use usage::{CompletionTokensDetails, ModalTokensDetails, PromptTokensDetails, UsageProbe};

/// 流式 chunk 探针：识别首个内容事件与随流 usage。
#[derive(Debug, Clone)]
pub struct ChunkProbe {
    pub choices: Vec<ChunkChoice>,
    pub usage: Option<UsageProbe>,
}

impl<'de> Deserialize<'de> for ChunkProbe {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct FastChoice {
            #[serde(default)]
            delta: ChunkDelta,
            #[serde(default)]
            usage: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct FastChunk {
            #[serde(default)]
            choices: Vec<FastChoice>,
            #[serde(default)]
            usage: Option<serde_json::Value>,
            #[serde(default)]
            timings: Option<serde_json::Value>,
        }
        // 单遍直接反序列化：绝大多数增量块不含 usage/timings，完全跳过整块 Value DOM 分配。
        let raw = FastChunk::deserialize(deserializer)?;
        let has_usage = raw.usage.is_some()
            || raw.timings.is_some()
            || raw.choices.iter().any(|c| c.usage.is_some());
        let usage = if has_usage {
            crate::token_usage::usage_from_chat_parts(
                raw.usage,
                raw.timings.as_ref(),
                raw.choices.iter().filter_map(|c| c.usage.as_ref()),
            )
        } else {
            None
        };
        let choices = raw
            .choices
            .into_iter()
            .map(|c| ChunkChoice { delta: c.delta })
            .collect();
        Ok(Self { choices, usage })
    }
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

/// [`ChunkProbe`] 解析不了的非标准 chunk（如 `content` 是分段数组）照样数产出：首字判定、
/// 空回复判定与无 usage 时的字符估算都靠它，一律按零产出会把真实输出漏计。
/// 口径同 [`ChunkProbe::has_output`] / [`ChunkProbe::content_chars`]，只是对字段类型宽容。
#[must_use]
pub fn chunk_output_lenient(chunk: &serde_json::Value) -> (bool, usize) {
    fn text_chars(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(text) => text.chars().count(),
            serde_json::Value::Array(parts) => parts
                .iter()
                .map(|part| match part {
                    serde_json::Value::String(text) => text.chars().count(),
                    serde_json::Value::Object(part) => part.get("text").map_or(0, text_chars),
                    _ => 0,
                })
                .sum(),
            _ => 0,
        }
    }
    let mut has_output = false;
    let mut chars = 0usize;
    let choices = chunk.get("choices").and_then(serde_json::Value::as_array);
    for delta in choices
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("delta"))
    {
        for field in ["content", "reasoning_content", "reasoning", "refusal"] {
            let count = delta.get(field).map_or(0, text_chars);
            has_output |= count > 0;
            chars = chars.saturating_add(count);
        }
        if let Some(calls) = delta.get("tool_calls").filter(|calls| !calls.is_null()) {
            has_output = true;
            for call in calls.as_array().into_iter().flatten() {
                let arguments = call.pointer("/function/arguments").map_or(0, text_chars);
                chars = chars.saturating_add(arguments);
            }
        }
    }
    (has_output, chars)
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

    /// 本 chunk 的产出字符数（无 usage 时的补全估算输入）：正文、推理、拒答，以及
    /// 工具调用参数——纯工具调用的流若在 usage 帧前中断，不数参数就只记 1 个补全 token。
    #[must_use]
    pub fn content_chars(&self) -> usize {
        let text: usize = self
            .choices
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
            .sum();
        let arguments: usize = self
            .choices
            .iter()
            .filter_map(|c| c.delta.tool_calls.as_ref()?.as_array())
            .flatten()
            .filter_map(|call| call.pointer("/function/arguments")?.as_str())
            .map(|s| s.chars().count())
            .sum();
        text.saturating_add(arguments)
    }
}

#[cfg(test)]
mod tests;
