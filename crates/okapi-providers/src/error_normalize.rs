//! 跨协议上游错误规范化与目标方言渲染。
//!
//! 当入口方言与上游方言不同（例如 OpenAI 客户端落到 Anthropic/Gemini/Bedrock 渠道，或
//! Anthropic 客户端落到 OpenAI 渠道）时，先提取上游结构化错误，再渲染成目标协议规范壳，
//! 避免把整段原始 JSON 塞进 `message` 或把非 OpenAI 壳直接透传给 OpenAI SDK。

use bytes::Bytes;
use serde_json::{Value, json};

/// 跨厂商归一化的语义错误类别（供跨方言渲染与诊断分析共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    ContextLength,
    ContentFilter,
    ModelUnavailable,
    RegionRestricted,
    RateLimit,
    Overloaded,
    Quota,
    Authentication,
    Permission,
    InvalidRequest,
    Server,
    Other,
}

impl ErrorCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContextLength => "context_length",
            Self::ContentFilter => "content_filter",
            Self::ModelUnavailable => "model_unavailable",
            Self::RegionRestricted => "region_restricted",
            Self::RateLimit => "rate_limit",
            Self::Overloaded => "overloaded",
            Self::Quota => "quota",
            Self::Authentication => "authentication",
            Self::Permission => "permission",
            Self::InvalidRequest => "invalid_request",
            Self::Server => "server",
            Self::Other => "other",
        }
    }

    const fn openai_type(self) -> &'static str {
        match self {
            Self::ContextLength
            | Self::ContentFilter
            | Self::RegionRestricted
            | Self::InvalidRequest => "invalid_request_error",
            Self::ModelUnavailable => "not_found_error",
            Self::RateLimit => "rate_limit_error",
            Self::Overloaded => "server_error",
            Self::Quota => "insufficient_quota",
            Self::Authentication => "authentication_error",
            Self::Permission => "permission_error",
            Self::Server | Self::Other => "api_error",
        }
    }

    const fn anthropic_type(self) -> &'static str {
        match self {
            Self::ContextLength | Self::ContentFilter | Self::InvalidRequest => {
                "invalid_request_error"
            }
            Self::ModelUnavailable => "not_found_error",
            Self::RegionRestricted | Self::Permission => "permission_error",
            Self::RateLimit | Self::Quota => "rate_limit_error",
            Self::Overloaded => "overloaded_error",
            Self::Authentication => "authentication_error",
            Self::Server | Self::Other => "api_error",
        }
    }

    const fn gemini_status(self) -> Option<&'static str> {
        match self {
            Self::ContextLength | Self::ContentFilter | Self::InvalidRequest => {
                Some("INVALID_ARGUMENT")
            }
            Self::ModelUnavailable => Some("NOT_FOUND"),
            Self::RegionRestricted => Some("FAILED_PRECONDITION"),
            Self::RateLimit | Self::Quota => Some("RESOURCE_EXHAUSTED"),
            Self::Overloaded => Some("UNAVAILABLE"),
            Self::Authentication => Some("UNAUTHENTICATED"),
            Self::Permission => Some("PERMISSION_DENIED"),
            Self::Server => Some("INTERNAL"),
            Self::Other => None,
        }
    }
}

/// 从上游任意方言错误体中提取的规范化错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedError {
    pub category: ErrorCategory,
    pub code: Option<String>,
    pub message: String,
    pub param: Option<String>,
}

const MAX_PARSE_BYTES: usize = 32 * 1024;
const MAX_MESSAGE_CHARS: usize = 2048;

impl NormalizedError {
    /// 解析 OpenAI / Anthropic / Gemini / AWS Bedrock / 纯文本上游错误体。
    #[must_use]
    pub fn parse(status: u16, body: &[u8]) -> Self {
        let parsed = (body.len() <= MAX_PARSE_BYTES)
            .then(|| serde_json::from_slice::<Value>(body).ok())
            .flatten();
        let Some(root) = parsed.as_ref().and_then(Value::as_object) else {
            let text = bounded(String::from_utf8_lossy(body).trim());
            let category = category_from_status(status);
            return Self {
                category,
                code: None,
                message: if text.is_empty() {
                    format!("upstream returned HTTP {status}")
                } else {
                    text
                },
                param: None,
            };
        };

        let err = root.get("error").and_then(Value::as_object);
        let code = first_non_empty(&[
            err.and_then(|e| e.get("code")),
            root.get("code"),
            root.get("__type"),
        ])
        .map(str::to_owned);
        let err_type = first_non_empty(&[
            err.and_then(|e| e.get("type")),
            err.and_then(|e| e.get("status")),
        ]);
        let param = first_non_empty(&[err.and_then(|e| e.get("param")), root.get("param")])
            .map(str::to_owned);
        let raw_msg = first_non_empty(&[
            err.and_then(|e| e.get("message")),
            root.get("message"),
            root.get("error").filter(|v| v.is_string()),
        ])
        .map_or_else(
            || {
                code.clone()
                    .or_else(|| err_type.map(str::to_owned))
                    .unwrap_or_else(|| format!("upstream returned HTTP {status}"))
            },
            bounded,
        );

        let reason = err
            .and_then(|e| e.get("details"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|d| d.get("reason").and_then(Value::as_str));

        let category = classify_category(status, code.as_deref(), err_type, reason, &raw_msg);
        let semantic_code = code.or_else(|| default_code(category).map(str::to_owned));
        Self {
            category,
            code: semantic_code,
            message: raw_msg,
            param,
        }
    }

    /// 渲染为 OpenAI 错误信封（`{"error":{"message","type","param","code"}}`）。
    #[must_use]
    pub fn render_openai(&self, status: u16) -> Bytes {
        let kind = if self.category == ErrorCategory::Other {
            category_from_status(status).openai_type()
        } else {
            self.category.openai_type()
        };
        Bytes::from(
            json!({
                "error": {
                    "message": self.message,
                    "type": kind,
                    "param": self.param,
                    "code": self.code,
                }
            })
            .to_string(),
        )
    }

    /// 渲染为 Anthropic 错误信封（`{"type":"error","error":{"type","message"}}`）。
    #[must_use]
    pub fn render_anthropic(&self, status: u16) -> Bytes {
        let kind = if self.category == ErrorCategory::Other {
            category_from_status(status).anthropic_type()
        } else {
            self.category.anthropic_type()
        };
        Bytes::from(
            json!({
                "type": "error",
                "error": {
                    "type": kind,
                    "message": self.message,
                }
            })
            .to_string(),
        )
    }

    /// 渲染为 Gemini `google.rpc.Status` 错误信封。
    #[must_use]
    pub fn render_gemini(&self, status: u16, fallback_grpc_status: &str) -> Bytes {
        let grpc = self
            .category
            .gemini_status()
            .unwrap_or(fallback_grpc_status);
        Bytes::from(
            json!({
                "error": {
                    "code": status,
                    "message": self.message,
                    "status": grpc,
                }
            })
            .to_string(),
        )
    }
}

fn first_non_empty<'a>(candidates: &[Option<&'a Value>]) -> Option<&'a str> {
    candidates
        .iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .find(|s| !s.is_empty())
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_MESSAGE_CHARS).collect()
}

fn default_code(category: ErrorCategory) -> Option<&'static str> {
    match category {
        ErrorCategory::ContextLength => Some("context_length_exceeded"),
        ErrorCategory::ContentFilter => Some("content_filter"),
        ErrorCategory::ModelUnavailable => Some("model_not_found"),
        ErrorCategory::RegionRestricted => Some("unsupported_country_region_territory"),
        ErrorCategory::RateLimit => Some("rate_limit_exceeded"),
        ErrorCategory::Overloaded => Some("overloaded"),
        ErrorCategory::Quota => Some("insufficient_quota"),
        ErrorCategory::Authentication => Some("invalid_api_key"),
        _ => None,
    }
}

const CATEGORY_TOKENS: &[(ErrorCategory, &[&str])] = &[
    (
        ErrorCategory::ContextLength,
        &[
            "context_length_exceeded",
            "max_tokens_exceeded",
            "string_above_max_length",
        ],
    ),
    (
        ErrorCategory::ContentFilter,
        &[
            "content_filter",
            "content_policy_violation",
            "safety",
            "prohibited_content",
            "recitation",
            "responsibleaipolicyviolation",
        ],
    ),
    (
        ErrorCategory::ModelUnavailable,
        &[
            "model_not_found",
            "deploymentnotfound",
            "resourcenotfoundexception",
        ],
    ),
    (
        ErrorCategory::RegionRestricted,
        &["unsupported_country_region_territory"],
    ),
    (
        ErrorCategory::Overloaded,
        &[
            "overloaded_error",
            "server_is_overloaded",
            "service_unavailable_error",
            "service_unavailable",
        ],
    ),
    (
        ErrorCategory::Quota,
        &[
            "insufficient_quota",
            "billing_hard_limit_reached",
            "billing_not_active",
            "billing_disabled",
            "arrearage",
            "insufficient_balance",
            "quota_exceeded",
        ],
    ),
    (
        ErrorCategory::RateLimit,
        &[
            "rate_limit_exceeded",
            "rate_limit_error",
            "resource_exhausted",
            "throttlingexception",
            "usage_limit_reached",
        ],
    ),
    (
        ErrorCategory::Authentication,
        &[
            "authentication_error",
            "invalid_api_key",
            "unauthenticated",
            "api_key_invalid",
            "unrecognizedclientexception",
            "expiredtokenexception",
        ],
    ),
    (
        ErrorCategory::Permission,
        &[
            "permission_error",
            "permission_denied",
            "accessdeniedexception",
        ],
    ),
    (
        ErrorCategory::InvalidRequest,
        &[
            "invalid_request_error",
            "invalid_argument",
            "validationexception",
        ],
    ),
];

const CATEGORY_PREFIXES: &[(ErrorCategory, &[&str])] = &[
    (
        ErrorCategory::ContextLength,
        &[
            "prompt is too long",
            "input is too long",
            "the input token count",
            "this model's maximum context length",
            "maximum context length",
        ],
    ),
    (
        ErrorCategory::RegionRestricted,
        &["user location is not supported"],
    ),
    (
        ErrorCategory::Quota,
        &[
            "your credit balance is too low",
            "insufficient balance",
            "you exceeded your current quota",
        ],
    ),
];

fn classify_category(
    status: u16,
    code: Option<&str>,
    err_type: Option<&str>,
    reason: Option<&str>,
    message: &str,
) -> ErrorCategory {
    let tokens = [code, err_type, reason];
    let has_token = |set: &[&str]| {
        tokens
            .iter()
            .flatten()
            .any(|t| set.iter().any(|s| t.eq_ignore_ascii_case(s)))
    };

    // 先判上下文超长 / 地区 / 欠费句首（例如 Anthropic 将 prompt_too_long / credit_balance 包在 invalid_request_error 里）
    for &(cat, prefixes) in CATEGORY_PREFIXES {
        if starts_with_any(message, prefixes) {
            return cat;
        }
    }
    for &(cat, set) in CATEGORY_TOKENS {
        if has_token(set) {
            return cat;
        }
    }
    category_from_status(status)
}

const fn category_from_status(status: u16) -> ErrorCategory {
    match status {
        400 | 422 => ErrorCategory::InvalidRequest,
        401 => ErrorCategory::Authentication,
        402 => ErrorCategory::Quota,
        403 => ErrorCategory::Permission,
        404 => ErrorCategory::ModelUnavailable,
        429 => ErrorCategory::RateLimit,
        529 => ErrorCategory::Overloaded,
        500..=599 => ErrorCategory::Server,
        _ => ErrorCategory::Other,
    }
}

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    let text = text.trim_start();
    prefixes.iter().any(|prefix| {
        text.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_anthropic_context_limit_cleanly_to_openai_envelope() {
        let raw = br#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#;
        let norm = NormalizedError::parse(400, raw);
        assert_eq!(norm.category, ErrorCategory::ContextLength);
        assert_eq!(norm.code.as_deref(), Some("context_length_exceeded"));
        let out: Value = serde_json::from_slice(&norm.render_openai(400)).unwrap();
        assert_eq!(out["error"]["type"], "invalid_request_error");
        assert_eq!(out["error"]["code"], "context_length_exceeded");
        assert_eq!(
            out["error"]["message"],
            "prompt is too long: 210000 tokens > 200000 maximum"
        );
    }

    #[test]
    fn translates_openai_error_to_clean_anthropic_envelope_without_json_in_message() {
        let raw = br#"{"error":{"message":"This model's maximum context length is 8192 tokens.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#;
        let norm = NormalizedError::parse(400, raw);
        assert_eq!(norm.category, ErrorCategory::ContextLength);
        let out: Value = serde_json::from_slice(&norm.render_anthropic(400)).unwrap();
        assert_eq!(out["type"], "error");
        assert_eq!(out["error"]["type"], "invalid_request_error");
        assert_eq!(
            out["error"]["message"],
            "This model's maximum context length is 8192 tokens."
        );
    }

    #[test]
    fn translates_gemini_location_restriction_and_bedrock_throttle() {
        let gemini = br#"{"error":{"code":400,"message":"User location is not supported for the API use.","status":"FAILED_PRECONDITION"}}"#;
        let norm = NormalizedError::parse(400, gemini);
        assert_eq!(norm.category, ErrorCategory::RegionRestricted);
        let openai: Value = serde_json::from_slice(&norm.render_openai(400)).unwrap();
        assert_eq!(
            openai["error"]["code"],
            "unsupported_country_region_territory"
        );
        assert_eq!(
            openai["error"]["message"],
            "User location is not supported for the API use."
        );

        let bedrock = br#"{"__type":"ThrottlingException","message":"Too many requests"}"#;
        let norm = NormalizedError::parse(429, bedrock);
        assert_eq!(norm.category, ErrorCategory::RateLimit);
        let gemini_out: Value =
            serde_json::from_slice(&norm.render_gemini(429, "INTERNAL")).unwrap();
        assert_eq!(gemini_out["error"]["status"], "RESOURCE_EXHAUSTED");
        assert_eq!(gemini_out["error"]["message"], "Too many requests");
    }
}
