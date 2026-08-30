//! okapi-api：OpenAI 兼容 DTO 与错误契约。
//!
//! 设计取舍（IMPLEMENTATION §3.7 / §8）：
//! - 请求**探针化解析**：只取路由与计费所需字段，原始 body 原样透传上游，
//!   不做整体反序列化-再序列化（保住 prompt cache 与未知字段的透传正确性）；
//! - 错误响应只携带 error_code（message 字段同置为 code，不含自然语言）。

pub mod chat;
pub mod error;
pub mod permissions;

pub use chat::{
    ChatRequestProbe, ChunkProbe, CompletionTokensDetails, MessageProbe, MessagesRequestProbe,
    PromptTokensDetails, ResponsesRequestProbe, UsageProbe,
};
pub use error::{ErrorBody, codes};
