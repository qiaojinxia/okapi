//! 预扣估算（M1 启发式）：chars/4 + 每消息固定开销。
//! tiktoken-rs 精确计数在 M2 接入（spawn_blocking，见 IMPLEMENTATION §12.3 压测项）；
//! 结算金额以上游 usage 为准，估算只影响预扣占用量。

pub fn estimate_prompt_tokens(prompt_chars: usize, message_count: usize) -> u32 {
    let overhead = message_count.saturating_mul(4).saturating_add(3);
    u32::try_from(prompt_chars / 4 + overhead).unwrap_or(u32::MAX)
}

/// 无上游 usage 时按已透传内容字符数兜底估算补全 tokens。
pub fn estimate_completion_tokens(content_chars: usize) -> u32 {
    u32::try_from((content_chars / 4).max(1)).unwrap_or(u32::MAX)
}
