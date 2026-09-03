//! 错误响应契约：`{"error":{"message":"<code>","type":"okapi_error","code":"<code>"}}`。
//! message 字段同置为 error_code（OpenAI 客户端要求非空 message；i18n 红线禁自然语言）。

use serde::Serialize;

/// 稳定错误码（前端 errors 命名空间做 i18n 映射）。
pub mod codes {
    pub const INVALID_API_KEY: &str = "invalid_api_key";
    pub const KEY_DISABLED: &str = "key_disabled";
    pub const MODEL_NOT_ALLOWED: &str = "model_not_allowed";
    pub const MODEL_NOT_FOUND: &str = "model_not_found";
    pub const INSUFFICIENT_QUOTA: &str = "insufficient_quota";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const NO_AVAILABLE_CHANNEL: &str = "no_available_channel";
    pub const EMPTY_COMPLETION: &str = "empty_completion";
    pub const UPSTREAM_ERROR: &str = "upstream_error";
    pub const UPSTREAM_TIMEOUT: &str = "upstream_timeout";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const INTERNAL_ERROR: &str = "internal_error";
    pub const STATS_DISABLED: &str = "stats_disabled";
    pub const PERMISSION_DENIED: &str = "permission_denied";
    pub const MEMBER_LIMIT_EXCEEDED: &str = "member_limit_exceeded";
    pub const NOT_FOUND: &str = "not_found";
    /// 用户给 key 选的分组不在其可选集合内（组可能存在，只是他没资格选）。
    pub const GROUP_NOT_SELECTABLE: &str = "group_not_selectable";
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorDetail {
    /// = code（不携带自然语言）。
    pub message: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ErrorBody {
    #[must_use]
    pub fn new(code: &str, param: Option<String>, request_id: Option<String>) -> Self {
        Self {
            error: ErrorDetail {
                message: code.to_owned(),
                kind: "okapi_error",
                code: code.to_owned(),
                param,
                request_id,
            },
        }
    }
}
