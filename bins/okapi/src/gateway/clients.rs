//! client_type 识别（#5277）：UA 解析为稳定枚举串，进 billing_records 与 CH 维度列。

use axum::http::HeaderMap;

const RULES: &[(&str, &str)] = &[
    ("claude-code", "claude-code"),
    ("claude_cli", "claude-code"),
    ("codex", "codex-cli"),
    ("openai-python", "openai-python"),
    ("openai-node", "openai-node"),
    ("lobechat", "lobechat"),
    ("cherry", "cherry-studio"),
    ("langchain", "langchain"),
    ("postmanruntime", "postman"),
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
