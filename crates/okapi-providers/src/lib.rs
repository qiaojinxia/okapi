//! okapi-providers：上游 LLM 适配。
//!
//! M1：OpenAI 方向（原生 + 一切 OpenAI 兼容上游）。模块按方向拆分、
//! 不做统一 IR（IMPLEMENTATION §4）；请求 body 原样透传，只动两处：模型名
//! 有映射时重写 `model`，流式请求补 `stream_options.include_usage`（缺它
//! 上游不返 usage，结算被迫走字符估算）。
//! M3：Anthropic 方向（/v1/messages 双向）+ Gemini 方向（generateContent 出向）。

pub mod anthropic;
pub mod convert;
pub mod custom_pass;
pub mod error;
pub mod gemini;
pub mod openai;
pub mod reasoning;
pub mod types;

pub use anthropic::AnthropicUpstream;
pub use custom_pass::PassUpstream;
pub use error::UpstreamError;
pub use gemini::GeminiUpstream;
pub use openai::{ChatResponse, OpenAiUpstream, StreamHandle, ensure_stream_usage, rewrite_model};
pub use reasoning::{ReasoningDirective, split_reasoning_suffix};
pub use types::ChatEvent;
