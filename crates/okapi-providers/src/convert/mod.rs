//! 协议方向转换（IMPLEMENTATION §4.1）：显式转换函数，不做统一 IR。
//! 文件按「请求方向」命名：`openai_to_anthropic` = OpenAI 协议客户端 → Anthropic 上游
//! （请求出向转换 + 响应/事件流回向转换打包在同一使用场景）。

pub mod anthropic_to_openai;
pub mod openai_to_anthropic;
pub mod openai_to_gemini;
pub mod responses_to_chat;
pub mod thinking;
