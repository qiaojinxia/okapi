//! 账号级错误：上游把「这把 key / 这个账号不能用了」包在 400 一类的请求错误状态里——余额不足、
//! 组织被停用、API key 无效。状态码看不出来，只能读正文；认不出来的话这类错误会被当成「请求本身有问题」
//! 原样回给客户端，key 一直被选中、一直失败，流量里落到它的那部分请求全部白白失败。
//!
//! 正文里常回显调用方可控的内容（模型名、工具名、字段值），所以只认**服务端自己写、不回显输入**的位置：
//! `error.code` / `error.type` / 顶层 `code` 这类枚举值，Gemini 的 `details[].reason`，以及
//! `error.message` / 顶层 `message` 的**句首**（回显的内容不会出现在句首）。不做整段正文的子串匹配——
//! 否则任何能指定模型名的用户都能把一把共用 key 打成失效。

use serde_json::Value;

/// 账号级故障的类别，对应 key 状态机里的两种终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFault {
    /// 余额 / 配额耗尽：充值之前不会好，冷却到次日再试。
    QuotaExhausted,
    /// 凭证或账号本身不可用（key 无效、组织停用）：只有人工处理才会恢复。
    Invalid,
}

/// 账号级错误都是几百字节的 JSON；更大的正文多半回显了请求，不读。
const MAX_BODY: usize = 16 * 1024;

/// `error.code` / `error.type` / 顶层 `code`：余额或配额耗尽。
const QUOTA_CODES: [&str; 4] = [
    // OpenAI：账单硬上限 / 账单未激活（HTTP 400 或 429，视账号）
    "billing_hard_limit_reached",
    "billing_not_active",
    "insufficient_quota",
    // 阿里云百炼：账号欠费（HTTP 400）
    "arrearage",
];
/// 句首短语：余额耗尽。
const QUOTA_PREFIXES: [&str; 1] = ["your credit balance is too low"];
/// Gemini `details[].reason`：key 无效或已过期。
const INVALID_REASONS: [&str; 1] = ["api_key_invalid"];
/// 句首短语：凭证 / 账号不可用。
const INVALID_PREFIXES: [&str; 5] = [
    "this organization has been disabled",
    "identity verification is required",
    "api key not valid",
    "api key expired",
    // AWS（Bedrock）：签名用的凭证无效
    "the security token included in the request is invalid",
];

/// 从上游错误正文认出账号级故障；认不出（或不是 JSON、过大）返回 None。
#[must_use]
pub fn classify(body: &[u8]) -> Option<AccountFault> {
    if body.is_empty() || body.len() > MAX_BODY {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    let root = value.as_object()?;
    let error = root.get("error").and_then(Value::as_object);

    let codes = [
        error.and_then(|e| e.get("code")),
        error.and_then(|e| e.get("type")),
        root.get("code"),
    ];
    let messages = [error.and_then(|e| e.get("message")), root.get("message")];
    let reasons = error
        .and_then(|e| e.get("details"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|detail| detail.get("reason").and_then(Value::as_str));

    // 先判更重的一类：两类同时命中时按「需要人工处理」算
    if reasons
        .clone()
        .any(|reason| any_equal(reason, &INVALID_REASONS))
        || messages
            .iter()
            .flatten()
            .filter_map(|m| m.as_str())
            .any(|m| any_prefix(m, &INVALID_PREFIXES))
    {
        return Some(AccountFault::Invalid);
    }
    if codes
        .iter()
        .flatten()
        .filter_map(|c| c.as_str())
        .any(|c| any_equal(c, &QUOTA_CODES))
        || messages
            .iter()
            .flatten()
            .filter_map(|m| m.as_str())
            .any(|m| any_prefix(m, &QUOTA_PREFIXES))
    {
        return Some(AccountFault::QuotaExhausted);
    }
    None
}

fn any_equal(value: &str, set: &[&str]) -> bool {
    set.iter().any(|item| value.eq_ignore_ascii_case(item))
}

/// 句首匹配（忽略大小写与前导空白）。`get(..n)` 遇到非字符边界返回 None，多字节文字不会 panic。
fn any_prefix(message: &str, prefixes: &[&str]) -> bool {
    let message = message.trim_start();
    prefixes.iter().any(|prefix| {
        message
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_str(body: &str) -> Option<AccountFault> {
        classify(body.as_bytes())
    }

    #[test]
    fn anthropic_credit_balance_is_quota_exhausted() {
        // HTTP 400 invalid_request_error —— Anthropic API key 预付费余额用完
        assert_eq!(
            classify_str(
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."},"request_id":"req_011C"}"#
            ),
            Some(AccountFault::QuotaExhausted)
        );
    }

    #[test]
    fn disabled_organization_needs_a_human() {
        assert_eq!(
            classify_str(
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"This organization has been disabled."}}"#
            ),
            Some(AccountFault::Invalid)
        );
        assert_eq!(
            classify_str(
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"  Identity verification is required to use this API."}}"#
            ),
            Some(AccountFault::Invalid),
            "句首允许前导空白与任意大小写"
        );
    }

    #[test]
    fn gemini_invalid_or_expired_key_is_invalid() {
        for message in [
            "API key not valid. Please pass a valid API key.",
            "API key expired. Please renew the API key.",
        ] {
            let body = format!(
                r#"{{"error":{{"code":400,"message":"{message}","status":"INVALID_ARGUMENT","details":[{{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID","domain":"googleapis.com","metadata":{{"service":"generativelanguage.googleapis.com"}}}}]}}}}"#
            );
            assert_eq!(
                classify_str(&body),
                Some(AccountFault::Invalid),
                "{message}"
            );
        }
        // 只有结构化的 reason（message 改了措辞）也认
        assert_eq!(
            classify_str(
                r#"{"error":{"code":400,"message":"Please check your key.","status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#
            ),
            Some(AccountFault::Invalid)
        );
    }

    #[test]
    fn closed_code_sets_cover_openai_compatible_vendors() {
        for body in [
            r#"{"error":{"message":"Billing hard limit has been reached","type":"invalid_request_error","param":null,"code":"billing_hard_limit_reached"}}"#,
            r#"{"error":{"message":"Your account is not active, please check your billing details on our website.","type":"invalid_request_error","code":"billing_not_active"}}"#,
            r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":null}}"#,
            // 百炼原生与兼容模式两种形状
            r#"{"code":"Arrearage","message":"Access denied, please make sure your account is in good standing.","request_id":"r"}"#,
            r#"{"error":{"code":"Arrearage","message":"Access denied, please make sure your account is in good standing.","type":"Arrearage"}}"#,
        ] {
            assert_eq!(
                classify_str(body),
                Some(AccountFault::QuotaExhausted),
                "{body}"
            );
        }
    }

    #[test]
    fn aws_invalid_security_token_is_invalid() {
        assert_eq!(
            classify_str(r#"{"message":"The security token included in the request is invalid."}"#),
            Some(AccountFault::Invalid)
        );
    }

    /// 防回显：模型名、工具名一类调用方可控的内容会被上游原样写进 `message`，
    /// 只要它不在句首，就不能让一把共用 key 因此被摘掉。
    #[test]
    fn echoed_caller_input_never_matches() {
        for body in [
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"model: Your credit balance is too low to access the Anthropic API is not a valid model"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"tools.0.name: 'This organization has been disabled.' does not match pattern"}}"#,
            r#"{"error":{"code":400,"message":"Invalid value at 'contents[0]': \"API key not valid\"","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.BadRequest"}]}}"#,
            // 枚举位置只认整词相等：包含不算
            r#"{"error":{"message":"x","type":"invalid_request_error","code":"model_billing_not_active_for_region"}}"#,
        ] {
            assert_eq!(classify_str(body), None, "{body}");
        }
    }

    #[test]
    fn ordinary_request_errors_are_not_account_faults() {
        for body in [
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.0.content: Input should be a valid list"}}"#,
            r#"{"error":{"code":400,"message":"Invalid JSON payload received.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.BadRequest"}]}}"#,
            r#"{"error":"upstream said no"}"#,
            "not json at all",
            "",
            "[]",
            "null",
        ] {
            assert_eq!(classify_str(body), None, "{body}");
        }
    }

    /// 失败转移的判据：账号级错误换渠道，哪怕被包在 400 / 404 / 413 / 422 里；
    /// 同一个状态码下的普通请求错误仍原样回给客户端。
    #[test]
    fn account_faults_fail_over_even_inside_request_error_statuses() {
        use crate::UpstreamError;
        use bytes::Bytes;
        let status = |code: u16, body: &str| UpstreamError::Status {
            status: code,
            body: Bytes::from(body.to_owned()),
            retry_after_secs: None,
        };
        let credit = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#;
        let plain = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages: Input should be a valid list"}}"#;
        for code in [400, 404, 413, 422] {
            let err = status(code, credit);
            assert!(err.retriable_before_first_token(), "{code}");
            assert_eq!(err.account_fault(), Some(AccountFault::QuotaExhausted));
            assert!(
                !status(code, plain).retriable_before_first_token(),
                "{code}"
            );
            assert_eq!(status(code, plain).account_fault(), None);
        }
        // 本来就换渠道的状态不受影响；没有正文的错误没有账号级故障可言
        assert!(status(429, plain).retriable_before_first_token());
        assert!(status(503, "").retriable_before_first_token());
        assert_eq!(UpstreamError::Timeout.account_fault(), None);
    }

    #[test]
    fn oversized_and_multibyte_bodies_are_safe() {
        let big = format!(
            r#"{{"error":{{"message":"Your credit balance is too low {}"}}}}"#,
            "x".repeat(MAX_BODY)
        );
        assert_eq!(classify(big.as_bytes()), None, "大正文多半回显了请求，不读");
        // 前缀长度落在多字节字符中间：不 panic、不命中
        assert_eq!(
            classify_str(
                r#"{"error":{"message":"余额不足，请充值后再试一次吧，谢谢你的耐心等待"}}"#
            ),
            None
        );
    }
}
