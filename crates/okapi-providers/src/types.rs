use okapi_api::UsageProbe;

/// 上游流式事件（gateway 原样透传 `raw`，元数据用于首字判定/计数/结算）。
#[derive(Debug, Clone)]
pub enum ChatEvent {
    Data {
        /// SSE data 负载原文（不重排字段，保住透传语义）。
        raw: String,
        /// SSE event 名（Anthropic 协议出口需要；None = 纯 data 行，OpenAI 风格）。
        event: Option<String>,
        /// 是否携带实际产出（内容/工具调用/拒答）——首字与空回复判定。
        has_output: bool,
        /// 本 chunk 可见内容字符数（无 usage 时的兜底估算输入）。
        content_chars: usize,
        /// 随流 usage（stream_options.include_usage 的最终 chunk）。
        usage: Option<UsageProbe>,
    },
    /// 收到 `[DONE]`。
    Done,
}
