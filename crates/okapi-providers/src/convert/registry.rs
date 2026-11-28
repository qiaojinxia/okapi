//! 协议转换注册表：显式方向转换规范、多跳链注册期首尾校验、质量分级与结构化诊断。
//!
//! 沿用 Okapi「不做统一 IR」红线（`convert/mod.rs`）：每个 [`ConverterSpec`] 显式声明
//! 客户端入口协议 `from`、上游协议 `to`、出向请求步链与回向响应步链；启动/首次访问时
//! 校验每条链首尾相接、无重名、无重复路由，把漏配或断链提前到启动/单测期暴露。

use super::{
    anthropic_to_openai as conv_a2o, chat_to_responses as conv_c2r, field_loss,
    gemini_to_openai as conv_g2o, openai_to_anthropic as conv_o2a, openai_to_gemini as conv_o2g,
    responses_to_chat as conv_r2c, thinking, tool_loss::ForeignTools,
};
use crate::error::UpstreamError;
use crate::inference::Response as InferenceResponse;
use crate::openai::{ChatResponse, StreamHandle};
use crate::registry::ApiFormat;
use crate::types::ChatEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// 协议转换质量分级（供路由诊断与管理端展示）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionQuality {
    /// 同方言原生直通（零协议转换，保真透传）。
    Native,
    /// 单跳显式转换（语义保真度高）。
    Good,
    /// 两跳组合转换（经由 ChatCompletions 中转）。
    Fair,
    /// 跨生态两跳转换（预期存在较高语义/特性损失）。
    Discouraged,
}

impl ConversionQuality {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Good => "good",
            Self::Fair => "fair",
            Self::Discouraged => "discouraged",
        }
    }
}

/// 结构化转换诊断严重度。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
}

/// 结构化协议转换诊断项（与 `dropped_tools` / `dropped_fields` 互补）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionDiagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub from: ApiFormat,
    pub to: ApiFormat,
}

/// 单次请求体转换上下文。
#[derive(Clone, Copy, Debug)]
pub struct ConvertContext<'a> {
    pub upstream_model: &'a str,
    pub completion_cap: u32,
    pub stream: bool,
}

/// 出向请求体单跳转换原语。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestStepKind {
    ChatToMessages,
    ChatToGemini,
    ChatToResponses,
    MessagesToChat,
    GeminiToChat,
    ResponsesToChat,
}

impl RequestStepKind {
    #[must_use]
    pub const fn from(self) -> ApiFormat {
        match self {
            Self::ChatToMessages | Self::ChatToGemini | Self::ChatToResponses => {
                ApiFormat::ChatCompletions
            }
            Self::MessagesToChat => ApiFormat::Messages,
            Self::GeminiToChat => ApiFormat::Gemini,
            Self::ResponsesToChat => ApiFormat::Responses,
        }
    }

    #[must_use]
    pub const fn to(self) -> ApiFormat {
        match self {
            Self::ChatToMessages => ApiFormat::Messages,
            Self::ChatToGemini => ApiFormat::Gemini,
            Self::ChatToResponses => ApiFormat::Responses,
            Self::MessagesToChat | Self::GeminiToChat | Self::ResponsesToChat => {
                ApiFormat::ChatCompletions
            }
        }
    }

    /// 执行当前单跳请求转换。
    pub fn apply(self, src: &Value, ctx: &ConvertContext<'_>) -> Result<Value, UpstreamError> {
        match self {
            Self::ChatToMessages => {
                conv_o2a::request_openai_to_anthropic_value(src, ctx.upstream_model, ctx.completion_cap)
            }
            Self::ChatToGemini => conv_o2g::request_openai_to_gemini_value(src),
            Self::ChatToResponses => {
                conv_c2r::request_chat_to_responses_value(src, ctx.upstream_model)
            }
            Self::MessagesToChat => {
                conv_a2o::request_anthropic_to_openai_value(src, ctx.upstream_model)
            }
            Self::GeminiToChat => {
                conv_g2o::request_gemini_to_openai_value(src, ctx.upstream_model, ctx.stream)
            }
            Self::ResponsesToChat => {
                conv_r2c::request_responses_to_chat_value(src, ctx.upstream_model)
            }
        }
    }
}

/// 回向响应单跳转换/元数据扫描原语（方向为 `upstream -> client`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseStepKind {
    PassthroughChat,
    PassthroughResponses,
    PassthroughResponsesCompact,
    MessagesPassthrough,
    GeminiPassthrough,
    MessagesToChat,
    GeminiToChat,
    ResponsesToChat,
    ChatToMessages,
    ChatToGemini,
    ChatToResponses,
}

impl ResponseStepKind {
    #[must_use]
    pub const fn from(self) -> ApiFormat {
        match self {
            Self::PassthroughChat
            | Self::ChatToMessages
            | Self::ChatToGemini
            | Self::ChatToResponses => ApiFormat::ChatCompletions,
            Self::PassthroughResponses | Self::ResponsesToChat => ApiFormat::Responses,
            Self::PassthroughResponsesCompact => ApiFormat::ResponsesCompact,
            Self::MessagesPassthrough | Self::MessagesToChat => ApiFormat::Messages,
            Self::GeminiPassthrough | Self::GeminiToChat => ApiFormat::Gemini,
        }
    }

    #[must_use]
    pub const fn to(self) -> ApiFormat {
        match self {
            Self::PassthroughChat
            | Self::MessagesToChat
            | Self::GeminiToChat
            | Self::ResponsesToChat => ApiFormat::ChatCompletions,
            Self::PassthroughResponses | Self::ChatToResponses => ApiFormat::Responses,
            Self::PassthroughResponsesCompact => ApiFormat::ResponsesCompact,
            Self::MessagesPassthrough | Self::ChatToMessages => ApiFormat::Messages,
            Self::GeminiPassthrough | Self::ChatToGemini => ApiFormat::Gemini,
        }
    }
}

/// 单条协议转换注册项：绑定请求侧与响应侧的完整声明式步链。
#[derive(Clone, Copy, Debug)]
pub struct ConverterSpec {
    pub name: &'static str,
    pub from: ApiFormat,
    pub to: ApiFormat,
    pub quality: ConversionQuality,
    pub request_steps: &'static [RequestStepKind],
    pub response_steps: &'static [ResponseStepKind],
}

impl ConverterSpec {
    #[must_use]
    pub const fn is_native(&self) -> bool {
        self.request_steps.is_empty()
    }

    /// 按声明的 `request_steps` 依次执行出向请求体转换。
    pub fn convert_request(
        &self,
        src: &Value,
        ctx: &ConvertContext<'_>,
    ) -> Result<Value, UpstreamError> {
        let Some((first, rest)) = self.request_steps.split_first() else {
            return Ok(src.clone());
        };
        let mut current = first.apply(src, ctx)?;
        for step in rest {
            current = step.apply(&current, ctx)?;
        }
        Ok(current)
    }

    /// 按声明的 `response_steps` 将上游响应转换为客户端方言形态（含 JSON 与流式 SSE）。
    pub fn convert_response(
        &self,
        raw: InferenceResponse,
        upstream_model: &str,
        thinking_to_content: bool,
    ) -> Result<ChatResponse, UpstreamError> {
        let Some((first, rest)) = self.response_steps.split_first() else {
            return Err(UpstreamError::Build("converter_missing_response_steps".into()));
        };
        let mut resp = apply_first_response_step(*first, raw, upstream_model, thinking_to_content)?;
        for step in rest {
            if matches!(step, ResponseStepKind::ChatToResponses) && thinking_to_content {
                resp = wrap_thinking_to_content(resp);
            }
            resp = apply_next_response_step(*step, resp, upstream_model)?;
        }
        if self.from == ApiFormat::ChatCompletions && thinking_to_content {
            resp = wrap_thinking_to_content(resp);
        }
        Ok(resp)
    }

    /// 生成结构化转换诊断（融合 `field_loss`、`tool_loss` 与转换质量分级）。
    #[must_use]
    pub fn diagnostics(&self, source: &Value, lost_tools: &ForeignTools) -> Vec<ConversionDiagnostic> {
        if self.is_native() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for field in field_loss::dropped_fields(self.from, self.to, source) {
            out.push(ConversionDiagnostic {
                code: "dropped_field".to_owned(),
                path: field.to_owned(),
                message: format!(
                    "field `{field}` has no equivalent when converting {} to {}",
                    self.from.as_str(),
                    self.to.as_str()
                ),
                severity: DiagnosticSeverity::Warning,
                from: self.from,
                to: self.to,
            });
        }
        for tool in &lost_tools.hosted {
            out.push(ConversionDiagnostic {
                code: "dropped_hosted_tool".to_owned(),
                path: format!("tools.{tool}"),
                message: format!("hosted tool `{tool}` dropped during cross-dialect conversion"),
                severity: DiagnosticSeverity::Info,
                from: self.from,
                to: self.to,
            });
        }
        for tool in &lost_tools.client {
            out.push(ConversionDiagnostic {
                code: "dropped_client_tool".to_owned(),
                path: format!("tools.{tool}"),
                message: format!("client tool `{tool}` dropped during cross-dialect conversion"),
                severity: DiagnosticSeverity::Warning,
                from: self.from,
                to: self.to,
            });
        }
        if self.quality == ConversionQuality::Discouraged {
            out.push(ConversionDiagnostic {
                code: "discouraged_conversion".to_owned(),
                path: self.name.to_owned(),
                message: format!(
                    "multi-hop conversion `{}` ({} -> {}) may degrade protocol fidelity",
                    self.name,
                    self.from.as_str(),
                    self.to.as_str()
                ),
                severity: DiagnosticSeverity::Info,
                from: self.from,
                to: self.to,
            });
        }
        out
    }
}

/// 内置协议转换规范表（唯一真源）。
pub const BUILT_IN_CONVERTERS: &[ConverterSpec] = &[
    // ---- 原生同方言透传 ----
    ConverterSpec {
        name: "openai_passthrough",
        from: ApiFormat::ChatCompletions,
        to: ApiFormat::ChatCompletions,
        quality: ConversionQuality::Native,
        request_steps: &[],
        response_steps: &[ResponseStepKind::PassthroughChat],
    },
    ConverterSpec {
        name: "anthropic_passthrough",
        from: ApiFormat::Messages,
        to: ApiFormat::Messages,
        quality: ConversionQuality::Native,
        request_steps: &[],
        response_steps: &[ResponseStepKind::MessagesPassthrough],
    },
    ConverterSpec {
        name: "gemini_passthrough",
        from: ApiFormat::Gemini,
        to: ApiFormat::Gemini,
        quality: ConversionQuality::Native,
        request_steps: &[],
        response_steps: &[ResponseStepKind::GeminiPassthrough],
    },
    ConverterSpec {
        name: "responses_passthrough",
        from: ApiFormat::Responses,
        to: ApiFormat::Responses,
        quality: ConversionQuality::Native,
        request_steps: &[],
        response_steps: &[ResponseStepKind::PassthroughResponses],
    },
    ConverterSpec {
        name: "responses_compact_passthrough",
        from: ApiFormat::ResponsesCompact,
        to: ApiFormat::ResponsesCompact,
        quality: ConversionQuality::Native,
        request_steps: &[],
        response_steps: &[ResponseStepKind::PassthroughResponsesCompact],
    },
    // ---- OpenAI ChatCompletions 入口 ----
    ConverterSpec {
        name: "openai_to_anthropic",
        from: ApiFormat::ChatCompletions,
        to: ApiFormat::Messages,
        quality: ConversionQuality::Good,
        request_steps: &[RequestStepKind::ChatToMessages],
        response_steps: &[ResponseStepKind::MessagesToChat],
    },
    ConverterSpec {
        name: "openai_to_gemini",
        from: ApiFormat::ChatCompletions,
        to: ApiFormat::Gemini,
        quality: ConversionQuality::Good,
        request_steps: &[RequestStepKind::ChatToGemini],
        response_steps: &[ResponseStepKind::GeminiToChat],
    },
    ConverterSpec {
        name: "openai_to_responses",
        from: ApiFormat::ChatCompletions,
        to: ApiFormat::Responses,
        quality: ConversionQuality::Good,
        request_steps: &[RequestStepKind::ChatToResponses],
        response_steps: &[ResponseStepKind::ResponsesToChat],
    },
    // ---- Anthropic Messages 入口 ----
    ConverterSpec {
        name: "anthropic_to_openai",
        from: ApiFormat::Messages,
        to: ApiFormat::ChatCompletions,
        quality: ConversionQuality::Good,
        request_steps: &[RequestStepKind::MessagesToChat],
        response_steps: &[ResponseStepKind::ChatToMessages],
    },
    ConverterSpec {
        name: "anthropic_to_responses",
        from: ApiFormat::Messages,
        to: ApiFormat::Responses,
        quality: ConversionQuality::Fair,
        request_steps: &[
            RequestStepKind::MessagesToChat,
            RequestStepKind::ChatToResponses,
        ],
        response_steps: &[
            ResponseStepKind::ResponsesToChat,
            ResponseStepKind::ChatToMessages,
        ],
    },
    // ---- Gemini GenerateContent 入口 ----
    ConverterSpec {
        name: "gemini_to_openai",
        from: ApiFormat::Gemini,
        to: ApiFormat::ChatCompletions,
        quality: ConversionQuality::Good,
        request_steps: &[RequestStepKind::GeminiToChat],
        response_steps: &[ResponseStepKind::ChatToGemini],
    },
    ConverterSpec {
        name: "gemini_to_anthropic",
        from: ApiFormat::Gemini,
        to: ApiFormat::Messages,
        quality: ConversionQuality::Discouraged,
        request_steps: &[
            RequestStepKind::GeminiToChat,
            RequestStepKind::ChatToMessages,
        ],
        response_steps: &[
            ResponseStepKind::MessagesToChat,
            ResponseStepKind::ChatToGemini,
        ],
    },
    ConverterSpec {
        name: "gemini_to_responses",
        from: ApiFormat::Gemini,
        to: ApiFormat::Responses,
        quality: ConversionQuality::Fair,
        request_steps: &[
            RequestStepKind::GeminiToChat,
            RequestStepKind::ChatToResponses,
        ],
        response_steps: &[
            ResponseStepKind::ResponsesToChat,
            ResponseStepKind::ChatToGemini,
        ],
    },
    // ---- OpenAI Responses 入口 ----
    ConverterSpec {
        name: "responses_to_openai",
        from: ApiFormat::Responses,
        to: ApiFormat::ChatCompletions,
        quality: ConversionQuality::Good,
        request_steps: &[RequestStepKind::ResponsesToChat],
        response_steps: &[ResponseStepKind::ChatToResponses],
    },
    ConverterSpec {
        name: "responses_to_anthropic",
        from: ApiFormat::Responses,
        to: ApiFormat::Messages,
        quality: ConversionQuality::Fair,
        request_steps: &[
            RequestStepKind::ResponsesToChat,
            RequestStepKind::ChatToMessages,
        ],
        response_steps: &[
            ResponseStepKind::MessagesToChat,
            ResponseStepKind::ChatToResponses,
        ],
    },
    ConverterSpec {
        name: "responses_to_gemini",
        from: ApiFormat::Responses,
        to: ApiFormat::Gemini,
        quality: ConversionQuality::Fair,
        request_steps: &[
            RequestStepKind::ResponsesToChat,
            RequestStepKind::ChatToGemini,
        ],
        response_steps: &[
            ResponseStepKind::GeminiToChat,
            ResponseStepKind::ChatToResponses,
        ],
    },
];

struct RegistryIndex {
    by_pair: HashMap<(ApiFormat, ApiFormat), &'static ConverterSpec>,
    by_name: HashMap<&'static str, &'static ConverterSpec>,
}

static REGISTRY: LazyLock<RegistryIndex> = LazyLock::new(|| {
    if let Err(err) = validate_specs(BUILT_IN_CONVERTERS) {
        panic!("invalid built-in protocol converter registry: {err}");
    }
    let mut by_pair = HashMap::with_capacity(BUILT_IN_CONVERTERS.len());
    let mut by_name = HashMap::with_capacity(BUILT_IN_CONVERTERS.len());
    for spec in BUILT_IN_CONVERTERS {
        by_pair.insert((spec.from, spec.to), spec);
        by_name.insert(spec.name, spec);
    }
    RegistryIndex { by_pair, by_name }
});

/// 校验转换表完整不变量：无重名、无重复 `(from, to)`、请求与响应步链均首尾相接。
pub fn validate_specs(specs: &[ConverterSpec]) -> Result<(), String> {
    let mut names = HashSet::with_capacity(specs.len());
    let mut pairs = HashSet::with_capacity(specs.len());
    for spec in specs {
        if spec.name.trim().is_empty() || !names.insert(spec.name) {
            return Err(format!("duplicate or empty converter name `{}`", spec.name));
        }
        if !pairs.insert((spec.from, spec.to)) {
            return Err(format!(
                "duplicate converter route {:?} -> {:?}",
                spec.from, spec.to
            ));
        }
        validate_request_chain(spec)?;
        validate_response_chain(spec)?;
    }
    Ok(())
}

fn validate_request_chain(spec: &ConverterSpec) -> Result<(), String> {
    if spec.from == spec.to {
        if !spec.request_steps.is_empty() || spec.quality != ConversionQuality::Native {
            return Err(format!(
                "native converter `{}` must have empty request_steps and Native quality",
                spec.name
            ));
        }
        return Ok(());
    }
    if spec.request_steps.is_empty() || spec.quality == ConversionQuality::Native {
        return Err(format!(
            "cross-dialect converter `{}` must declare request_steps and non-Native quality",
            spec.name
        ));
    }
    let mut cursor = spec.from;
    for step in spec.request_steps {
        if step.from() != cursor {
            return Err(format!(
                "converter `{}` request step {:?} expects {:?}, got {:?}",
                spec.name,
                step,
                step.from(),
                cursor
            ));
        }
        cursor = step.to();
    }
    if cursor != spec.to {
        return Err(format!(
            "converter `{}` request chain ends at {:?}, expected {:?}",
            spec.name, cursor, spec.to
        ));
    }
    Ok(())
}

fn validate_response_chain(spec: &ConverterSpec) -> Result<(), String> {
    if spec.response_steps.is_empty() {
        return Err(format!("converter `{}` has empty response_steps", spec.name));
    }
    let mut cursor = spec.to;
    for step in spec.response_steps {
        if step.from() != cursor {
            return Err(format!(
                "converter `{}` response step {:?} expects {:?}, got {:?}",
                spec.name,
                step,
                step.from(),
                cursor
            ));
        }
        cursor = step.to();
    }
    if cursor != spec.from {
        return Err(format!(
            "converter `{}` response chain ends at {:?}, expected {:?}",
            spec.name, cursor, spec.from
        ));
    }
    Ok(())
}

/// 按 `(客户端入口协议, 上游目标协议)` 查找注册的转换器。
#[must_use]
pub fn lookup(from: ApiFormat, to: ApiFormat) -> Option<&'static ConverterSpec> {
    REGISTRY.by_pair.get(&(from, to)).copied()
}

/// 按转换器名称查找（供声明式 `advanced_routes` 引用）。
#[must_use]
pub fn lookup_by_name(name: &str) -> Option<&'static ConverterSpec> {
    REGISTRY.by_name.get(name.trim()).copied()
}

/// 列出全部已注册的内置转换规范（会触发注册表完整性校验）。
#[must_use]
pub fn list_specs() -> &'static [ConverterSpec] {
    let _ = &*REGISTRY;
    BUILT_IN_CONVERTERS
}

/// thinking-to-content 渠道开关：OpenAI 方言出口的 reasoning 转 `<think>` 正文。
#[must_use]
pub fn wrap_thinking_to_content(resp: ChatResponse) -> ChatResponse {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => ChatResponse::Json {
            status,
            upstream_request_id,
            body: thinking::rewrite_json(&body),
            usage,
        },
        ChatResponse::Stream(h) => {
            let mut st = thinking::ThinkingToContent::new();
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(st.step(item)));
            ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            })
        }
    }
}

/// chat 形状 → Responses 方言（`response.created/.output_text.delta/.completed` 事件骨架）。
pub fn wrap_chat_as_responses(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    let resp = surface_stream_failures(resp);
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => {
            let (body, parsed_usage) = conv_r2c::response_chat_to_responses(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                // Preserve upstream probe (including invalid-usage marker) across conversion hops.
                usage: usage.or(parsed_usage),
            })
        }
        ChatResponse::Stream(h) => {
            let mut st = conv_r2c::ChatStreamToResponses::new(upstream_model);
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(st.step(item)));
            Ok(ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            }))
        }
    }
}

fn wrap_chat_as_anthropic(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => {
            let (body, parsed_usage) = conv_a2o::response_openai_to_anthropic(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage: usage.or(parsed_usage),
            })
        }
        ChatResponse::Stream(h) => {
            let mut st = conv_a2o::OaiStreamToAnthropic::new(upstream_model);
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(st.step(item)));
            Ok(ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            }))
        }
    }
}

/// 在跨方言流式转换前，将流内尚未产出正文时的错误帧显式提升为 `Err(UpstreamError)`。
#[must_use]
pub fn surface_stream_failures(resp: ChatResponse) -> ChatResponse {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Stream(h) => ChatResponse::Stream(StreamHandle {
            upstream_request_id: h.upstream_request_id,
            events: Box::pin(h.events.map(|item| {
                if let Ok(ChatEvent::Data {
                    raw,
                    has_output: false,
                    ..
                }) = &item
                    && let Some(failure) = crate::stream_failure::detect(raw)
                    && let Ok(error) = failure.into_upstream_error()
                {
                    return Err(error);
                }
                item
            })),
        }),
        json @ ChatResponse::Json { .. } => json,
    }
}

fn passthrough_messages(resp: crate::anthropic::MessagesResponse) -> ChatResponse {
    use futures::StreamExt as _;
    match resp {
        crate::anthropic::MessagesResponse::Json {
            status,
            upstream_request_id,
            body,
        } => {
            let usage = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| conv_o2a::usage_from_anthropic(v.get("usage")));
            ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            }
        }
        crate::anthropic::MessagesResponse::Stream(h) => {
            let mut scanner = crate::anthropic::MetaScanner::new();
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(scanner.scan(item)));
            ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            })
        }
    }
}

fn passthrough_gemini(resp: crate::gemini::GeminiResponse) -> ChatResponse {
    use futures::StreamExt as _;
    match resp {
        crate::gemini::GeminiResponse::Json {
            status,
            upstream_request_id,
            body,
        } => {
            let usage = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| conv_o2g::usage_from_gemini(v.get("usageMetadata")));
            ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
            }
        }
        crate::gemini::GeminiResponse::Stream(h) => {
            let mut scanner = crate::gemini::MetaScanner::new();
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(scanner.scan(item)));
            ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            })
        }
    }
}

fn apply_first_response_step(
    step: ResponseStepKind,
    raw: InferenceResponse,
    upstream_model: &str,
    thinking_to_content: bool,
) -> Result<ChatResponse, UpstreamError> {
    let mismatch = || UpstreamError::Build("adapter_response_mismatch".into());
    match (step, raw) {
        (
            ResponseStepKind::PassthroughChat
            | ResponseStepKind::PassthroughResponses
            | ResponseStepKind::PassthroughResponsesCompact,
            InferenceResponse::OpenAi(resp),
        ) => Ok(resp),
        (ResponseStepKind::MessagesPassthrough, InferenceResponse::Messages(resp)) => {
            Ok(passthrough_messages(resp))
        }
        (ResponseStepKind::GeminiPassthrough, InferenceResponse::Gemini(resp)) => {
            Ok(passthrough_gemini(resp))
        }
        (ResponseStepKind::MessagesToChat, InferenceResponse::Messages(resp)) => {
            conv_o2a::wrap_messages(resp, upstream_model)
        }
        (ResponseStepKind::GeminiToChat, InferenceResponse::Gemini(resp)) => {
            conv_o2g::wrap_generate(resp, upstream_model)
        }
        (ResponseStepKind::ResponsesToChat, InferenceResponse::OpenAi(resp)) => {
            conv_c2r::wrap_responses_as_chat(surface_stream_failures(resp), upstream_model)
        }
        (ResponseStepKind::ChatToMessages, InferenceResponse::OpenAi(resp)) => {
            wrap_chat_as_anthropic(surface_stream_failures(resp), upstream_model)
        }
        (ResponseStepKind::ChatToGemini, InferenceResponse::OpenAi(resp)) => {
            conv_g2o::wrap_chat_as_gemini(surface_stream_failures(resp), upstream_model)
        }
        (ResponseStepKind::ChatToResponses, InferenceResponse::OpenAi(resp)) => {
            let resp = if thinking_to_content {
                wrap_thinking_to_content(resp)
            } else {
                resp
            };
            wrap_chat_as_responses(resp, upstream_model)
        }
        _ => Err(mismatch()),
    }
}

fn apply_next_response_step(
    step: ResponseStepKind,
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    match step {
        ResponseStepKind::ChatToMessages => {
            wrap_chat_as_anthropic(surface_stream_failures(resp), upstream_model)
        }
        ResponseStepKind::ChatToGemini => {
            conv_g2o::wrap_chat_as_gemini(surface_stream_failures(resp), upstream_model)
        }
        ResponseStepKind::ChatToResponses => wrap_chat_as_responses(resp, upstream_model),
        _ => Err(UpstreamError::Build("converter_invalid_response_hop".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn built_in_registry_passes_validation_and_matches_api_contract_list() {
        assert_eq!(validate_specs(BUILT_IN_CONVERTERS), Ok(()));
        assert_eq!(list_specs().len(), 16);

        for &name in okapi_api::contracts::ADVANCED_ROUTE_CONVERTERS {
            if matches!(name, "auto" | "passthrough") {
                continue;
            }
            assert!(
                lookup_by_name(name).is_some(),
                "contract converter `{name}` must exist in provider registry"
            );
        }
        for spec in BUILT_IN_CONVERTERS {
            assert!(
                okapi_api::contracts::ADVANCED_ROUTE_CONVERTERS.contains(&spec.name),
                "built-in converter `{}` missing from okapi_api::contracts::ADVANCED_ROUTE_CONVERTERS",
                spec.name
            );
            assert_eq!(lookup(spec.from, spec.to).map(|s| s.name), Some(spec.name));
        }
    }

    #[test]
    fn validator_rejects_broken_chains_and_duplicates() {
        let broken_request = [ConverterSpec {
            name: "broken_req",
            from: ApiFormat::Responses,
            to: ApiFormat::Messages,
            quality: ConversionQuality::Fair,
            // 缺少 ResponsesToChat，直接从 ChatCompletions 起步
            request_steps: &[RequestStepKind::ChatToMessages],
            response_steps: &[
                ResponseStepKind::MessagesToChat,
                ResponseStepKind::ChatToResponses,
            ],
        }];
        assert!(validate_specs(&broken_request).is_err());

        let broken_response = [ConverterSpec {
            name: "broken_resp",
            from: ApiFormat::Responses,
            to: ApiFormat::Messages,
            quality: ConversionQuality::Fair,
            request_steps: &[
                RequestStepKind::ResponsesToChat,
                RequestStepKind::ChatToMessages,
            ],
            // 回向链末尾落到 ChatCompletions 而不是 Responses
            response_steps: &[ResponseStepKind::MessagesToChat],
        }];
        assert!(validate_specs(&broken_response).is_err());

        let duplicate_pair = [
            ConverterSpec {
                name: "dup_a",
                from: ApiFormat::ChatCompletions,
                to: ApiFormat::Messages,
                quality: ConversionQuality::Good,
                request_steps: &[RequestStepKind::ChatToMessages],
                response_steps: &[ResponseStepKind::MessagesToChat],
            },
            ConverterSpec {
                name: "dup_b",
                from: ApiFormat::ChatCompletions,
                to: ApiFormat::Messages,
                quality: ConversionQuality::Good,
                request_steps: &[RequestStepKind::ChatToMessages],
                response_steps: &[ResponseStepKind::MessagesToChat],
            },
        ];
        assert!(validate_specs(&duplicate_pair).is_err());
    }

    #[test]
    fn multi_hop_request_conversion_executes_registered_steps() {
        let spec = lookup_by_name("responses_to_anthropic").expect("registered");
        assert_eq!(spec.quality, ConversionQuality::Fair);
        let src = json!({
            "model": "gpt-5",
            "input": "hello from responses"
        });
        let ctx = ConvertContext {
            upstream_model: "claude-sonnet-4-5",
            completion_cap: 1024,
            stream: false,
        };
        let converted = spec.convert_request(&src, &ctx).expect("converts");
        assert_eq!(converted["model"], "claude-sonnet-4-5");
        assert_eq!(converted["max_tokens"], 1024);
        assert_eq!(converted["messages"][0]["role"], "user");
    }

    #[test]
    fn structured_diagnostics_merge_field_loss_tool_loss_and_quality() {
        let spec = lookup_by_name("gemini_to_anthropic").expect("registered");
        assert_eq!(spec.quality, ConversionQuality::Discouraged);
        let src = json!({
            "cachedContent": "cachedContents/abc",
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
        });
        let lost = ForeignTools {
            hosted: vec!["google_search".to_owned()],
            client: vec!["local_shell".to_owned()],
        };
        let diags = spec.diagnostics(&src, &lost);
        assert!(diags.iter().any(|d| d.code == "dropped_field" && d.path == "cached_content"));
        assert!(
            diags
                .iter()
                .any(|d| d.code == "dropped_hosted_tool" && d.severity == DiagnosticSeverity::Info)
        );
        assert!(
            diags.iter().any(
                |d| d.code == "dropped_client_tool" && d.severity == DiagnosticSeverity::Warning
            )
        );
        assert!(diags.iter().any(|d| d.code == "discouraged_conversion"));
    }
}
