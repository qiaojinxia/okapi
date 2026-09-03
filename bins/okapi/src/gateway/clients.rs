//! client_type 识别（#5277）：UA 解析为稳定枚举串，进 billing_records 与 CH 维度列。
//!
//! 规则按**特异性降序**排列且取首次命中：专用工具（Claude Code / Codex CLI / IDE
//! 插件）在前，它们的 UA 往往同时带着底层 SDK 的标识（Claude Code 走
//! anthropic SDK、Cursor 走 openai SDK），先匹配 SDK 会把工具归错类。

use axum::http::HeaderMap;

const RULES: &[(&str, &str)] = &[
    // 编码智能体 / CLI
    ("claude-code", "claude-code"),
    ("claude_cli", "claude-code"),
    ("codex", "codex-cli"),
    ("gemini-cli", "gemini-cli"),
    ("cursor", "cursor"),
    ("cline", "cline"),
    ("continue", "continue"),
    // 桌面 / Web 聊天客户端
    ("lobechat", "lobechat"),
    ("lobe-chat", "lobechat"),
    ("cherry", "cherry-studio"),
    ("chatbox", "chatbox"),
    ("nextchat", "nextchat"),
    ("chatgpt-next-web", "nextchat"),
    ("sillytavern", "sillytavern"),
    ("immersive", "immersive-translate"),
    ("dify", "dify"),
    // 官方 SDK（Stainless 生成，UA 形如 `OpenAI/Python 1.x` / `Anthropic/JS 0.x`）
    ("openai/python", "openai-python"),
    ("openai-python", "openai-python"),
    ("openai/js", "openai-node"),
    ("openai-node", "openai-node"),
    ("anthropic/python", "anthropic-python"),
    ("anthropic/js", "anthropic-node"),
    ("langchain", "langchain"),
    ("litellm", "litellm"),
    // 通用 HTTP 客户端（最后兜底：任何专用工具都可能建在它们之上）
    ("postmanruntime", "postman"),
    ("python-requests", "python-requests"),
    ("python-httpx", "python-httpx"),
    ("axios", "axios"),
    ("node-fetch", "node"),
    ("undici", "node"),
    ("go-http-client", "go"),
    ("okhttp", "okhttp"),
    ("curl", "curl"),
];

#[must_use]
pub fn detect_client_type(headers: &HeaderMap) -> &'static str {
    let Some(ua) = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
    else {
        return "";
    };
    let ua = ua.to_ascii_lowercase();
    for (needle, label) in RULES {
        if ua.contains(needle) {
            return label;
        }
    }
    ""
}

/// 客户端 IP：CDN/代理头按序取首个有效值（§14.2；可配名单列 backlog，
/// 常量序覆盖 Cloudflare/通用反代/直连）。
#[must_use]
pub fn detect_client_ip(headers: &axum::http::HeaderMap) -> Option<String> {
    for name in [
        "true-client-ip",
        "cf-connecting-ip",
        "x-real-ip",
        "x-forwarded-for",
    ] {
        if let Some(value) = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.parse::<std::net::IpAddr>().is_ok())
        {
            return Some(value.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::detect_client_type;
    use axum::http::{HeaderMap, HeaderValue, header};

    fn detect(ua: &str) -> &'static str {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, HeaderValue::from_str(ua).unwrap());
        detect_client_type(&headers)
    }

    /// 官方 SDK 的真实 UA 形态（Stainless 生成器）——此前规则表写的是
    /// `openai-python`，与真实 UA `OpenAI/Python` 永远匹配不上，分布图里
    /// 最大的一类流量会全部落进"未知"。
    #[test]
    fn stainless_sdk_user_agents_are_recognised() {
        assert_eq!(detect("OpenAI/Python 1.54.3"), "openai-python");
        assert_eq!(detect("OpenAI/JS 4.73.0"), "openai-node");
        assert_eq!(detect("Anthropic/Python 0.39.0"), "anthropic-python");
        assert_eq!(detect("Anthropic/JS 0.32.1"), "anthropic-node");
    }

    /// 专用工具建在 SDK 之上，UA 同时含两者标识时必须归到工具而非 SDK。
    #[test]
    fn specific_tools_win_over_underlying_sdk() {
        assert_eq!(
            detect("claude-code/1.0.2 Anthropic/JS 0.32.1"),
            "claude-code"
        );
        assert_eq!(detect("Cursor/0.45 OpenAI/JS 4.7"), "cursor");
        assert_eq!(detect("codex_cli_rs/0.9.0"), "codex-cli");
    }

    #[test]
    fn generic_http_clients_are_last_resort_and_unknown_is_empty() {
        assert_eq!(detect("python-requests/2.32.0"), "python-requests");
        assert_eq!(detect("curl/8.6.0"), "curl");
        assert_eq!(detect("Mozilla/5.0 (X11; Linux) SomeBrowser/1.0"), "");
        assert_eq!(detect_client_type(&HeaderMap::new()), "");
    }
}
