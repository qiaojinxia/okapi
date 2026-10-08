//! 协议方向转换（IMPLEMENTATION §4.1）：显式转换函数，不做统一 IR。
//! 文件按「请求方向」命名：`openai_to_anthropic` = OpenAI 协议客户端 → Anthropic 上游
//! （请求出向转换 + 响应/事件流回向转换打包在同一使用场景）。

pub mod anthropic_to_openai;
pub mod gemini_to_openai;
pub mod openai_to_anthropic;
pub mod openai_to_gemini;
pub mod responses_to_chat;
pub mod thinking;

mod gemini_usage;

use crate::error::UpstreamError;
use serde_json::{Value, json};

/// OpenAI `tool_calls[].function.arguments`（JSON 字符串）→ 对端要求的对象形参数。
/// 空串按无参数。解析不出对象（典型是上一轮被 max_tokens 截断的调用）时把原文放进
/// `_raw_arguments`：换成 `{}` 会让模型以为当时没传参数，原文还在上下文里它才看得出断在哪。
pub(crate) fn tool_arguments(arguments: Option<&str>) -> Value {
    let raw = arguments.unwrap_or_default().trim();
    if raw.is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => value,
        _ => json!({ "_raw_arguments": raw }),
    }
}

/// 对端表达不了的 content part。直接报错而不是丢掉：丢掉后模型在看不到附件的情况下作答，
/// 整条 user turn 被丢时还会把上一条 assistant 当成预填接着写。网关把它映射成 400，param = part 类型。
pub(crate) fn unsupported_part(kind: &str) -> UpstreamError {
    UpstreamError::Build(format!("{UNSUPPORTED_CONTENT_PREFIX}{kind}"))
}

/// 流式 `tool_calls` 分片的槽位上限（每个槽对应一个工具调用）。
pub(crate) const MAX_STREAM_TOOLS: usize = 128;

/// 流式 `tool_calls` 分片 → 槽位。兼容上游常省略 `index`（单工具时尤甚）：缺省时按 id 认槽，
/// 新 id 开新槽，没有 id 的续写最近一个槽。显式但非法（非整数 / 越界）的返回 `None`。
pub(crate) fn stream_tool_slot(call: &Value, ids: &mut Vec<String>) -> Option<usize> {
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let slot = match call.get("index") {
        Some(index) => usize::try_from(index.as_u64()?).ok()?,
        None => match id {
            Some(id) => ids.iter().position(|seen| seen == id).unwrap_or(ids.len()),
            None => ids.len().saturating_sub(1),
        },
    };
    if slot >= MAX_STREAM_TOOLS {
        return None;
    }
    if ids.len() <= slot {
        ids.resize(slot + 1, String::new());
    }
    if let Some(id) = id
        && ids[slot].is_empty()
    {
        id.clone_into(&mut ids[slot]);
    }
    Some(slot)
}

/// [`unsupported_part`] 的原因前缀，网关据此把构造失败映射成带 param 的 400。
pub const UNSUPPORTED_CONTENT_PREFIX: &str = "unsupported_content:";
