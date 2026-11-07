//! 上游回复的判定（各入口共用）：一次失败该记到 key 状态机的哪一类，2xx 的体是不是 JSON 对象。
//!
//! 原先放在 chat 里，媒体端点与图片流都要反过来引用 chat；挪出来后各入口平级引用这里。
use bytes::Bytes;
use okapi_providers::UpstreamError;
use okapi_providers::account_fault::AccountFault;
use okapi_store::channels::KeyFailure;

/// 上游错误 → key 状态机类别（§3.6 重试矩阵）。
/// 注意顺序：insufficient_quota 判定先于 429——OpenAI 风格的
/// `429 + insufficient_quota` 语义是配额耗尽（冷却到次日），不是限速（60s）。
pub(crate) fn failure_kind_of(err: &UpstreamError) -> KeyFailure {
    match err {
        UpstreamError::Status { status, body, .. }
            if *status == 402 || (*status == 429 && body_says_insufficient_quota(body)) =>
        {
            KeyFailure::QuotaExhausted
        }
        UpstreamError::Status { status: 529, .. } => KeyFailure::RateLimited {
            retry_after_secs: Some(err.retry_after_secs().unwrap_or(600)),
        },
        UpstreamError::Status { status: 429, .. } => KeyFailure::RateLimited {
            retry_after_secs: err.retry_after_secs(),
        },
        UpstreamError::Status { status: 401, .. } => KeyFailure::Invalid,
        // 403 只在 body 明说凭证问题时才算凭证失效，否则按资源级失败回退，不改变 key 健康（详见
        // `body_says_credential_rejected`）
        UpstreamError::Status {
            status: 403, body, ..
        } if body_says_credential_rejected(body) => KeyFailure::Invalid,
        UpstreamError::Status {
            status: 500..=599, ..
        } => KeyFailure::Transient,
        // 连接阶段失败：凭证无从判断；走了代理时归给代理的被动熔断（§11.41）
        UpstreamError::Unreachable { proxy_hop, .. } => KeyFailure::Unreachable {
            proxy_hop: *proxy_hop,
        },
        // 状态码看不出来、正文明说账号不能用了（余额耗尽 / 组织停用 / key 无效，常被包成 400 或
        // 不带关键词的 403）：这把 key 要按账号级故障登记，否则它会一直被选中、一直失败
        UpstreamError::Status { .. } => match err.account_fault() {
            Some(AccountFault::QuotaExhausted) => KeyFailure::QuotaExhausted,
            Some(AccountFault::Invalid) => KeyFailure::Invalid,
            None => KeyFailure::Request,
        },
        UpstreamError::Connect(_)
        | UpstreamError::Timeout
        | UpstreamError::Stream(_)
        | UpstreamError::Session { .. }
        | UpstreamError::Build(_) => KeyFailure::Request,
    }
}

/// 上游的 403 是不是真的在说「这把凭证不认」。
///
/// 401 = 没通过认证 → 凭证必然有问题；403 = 认证过了但不被允许，本质是**按资源**的判定。
/// 聚合型上游普遍拿 403 表达「这个模型你的套餐没开通」（实测到的原文：
/// `access_denied / Deposit required to unlock premium models`），而 key 本身完全有效。
/// 此前 401 与 403 一并判 `Invalid`（`status=6`，无冷却、不自愈，控制面也没有复活入口），
/// 于是调一次未开通的模型就把该渠道**所有**模型打死，只能靠重置凭证救回来。
/// 故 403 改为：body 明说凭证问题才算失效，否则不改变 key 健康状态。
fn body_says_credential_rejected(body: &Bytes) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
        "invalid_api_key",
        "invalid api key",
        "incorrect api key",
        "invalid_authentication",
        "authentication_error",
        "invalid token",
        "api key not valid",
        "unauthorized",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn body_says_insufficient_quota(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let err = v.get("error")?;
            let code = err.get("code").and_then(|c| c.as_str()).unwrap_or("");
            let kind = err.get("type").and_then(|t| t.as_str()).unwrap_or("");
            Some(code.contains("insufficient_quota") || kind.contains("insufficient_quota"))
        })
        .unwrap_or(false)
}

/// 对话 / 消息 / Responses / Gemini / embeddings / rerank / count_tokens 的 2xx 响应都是 JSON 对象。
/// 体不是时返回它是什么（诊断里只留种类与长度，不留内容——体里可能回显提示词）。
///
/// 站点的网页前端、网关的错误页对任何未知路径回 `200 text/html` 是常态：渠道地址缺了版本段（`/v1`）、
/// 指到了网页端口时就是这样。把它当成功转给客户端再按估算计费是错的。UTF-8 BOM 与前导空白不算错。
pub(crate) fn non_json_kind(body: &[u8]) -> Option<&'static str> {
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    match body.iter().find(|b| !b.is_ascii_whitespace()) {
        None => Some("empty"),
        Some(b'<') => Some("html"),
        Some(b'{') if serde_json::from_slice::<serde::de::IgnoredAny>(body).is_ok() => None,
        Some(b'{') => Some("malformed_json"),
        Some(_) => Some("other"),
    }
}

/// [`non_json_kind`] 在诊断里的样子：种类 + 长度。
pub(crate) fn non_json_failure(kind: &str, len: usize) -> UpstreamError {
    UpstreamError::Stream(format!("upstream_not_json ({kind}, {len} bytes)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(status: u16, body: &str) -> UpstreamError {
        UpstreamError::Status {
            status,
            body: Bytes::copy_from_slice(body.as_bytes()),
            retry_after_secs: None,
        }
    }

    #[test]
    fn request_errors_and_resource_permissions_do_not_change_account_health() {
        for error in [
            UpstreamError::Connect("dns".into()),
            UpstreamError::Timeout,
            UpstreamError::Stream("transport".into()),
            status(403, r#"{"error":{"code":"access_denied"}}"#),
            status(408, ""),
        ] {
            assert!(matches!(failure_kind_of(&error), KeyFailure::Request));
        }
        assert!(matches!(
            failure_kind_of(&status(401, "")),
            KeyFailure::Invalid
        ));
        assert!(matches!(
            failure_kind_of(&status(403, r#"{"error":{"code":"invalid_api_key"}}"#)),
            KeyFailure::Invalid
        ));
        assert!(matches!(
            failure_kind_of(&status(429, r#"{"error":{"code":"insufficient_quota"}}"#)),
            KeyFailure::QuotaExhausted
        ));
        assert!(matches!(
            failure_kind_of(&status(529, "")),
            KeyFailure::RateLimited {
                retry_after_secs: Some(600)
            }
        ));
        assert!(matches!(
            failure_kind_of(&status(503, "")),
            KeyFailure::Transient
        ));
    }

    /// Google 系的 429 把等待时间放在正文的 `RetryInfo.retryDelay`：key 的冷却按它走，不再落到缺省 60 秒。
    #[test]
    fn google_retry_info_sets_the_key_cooldown() {
        let body = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"34s"}]}}"#;
        assert!(matches!(
            failure_kind_of(&status(429, body)),
            KeyFailure::RateLimited {
                retry_after_secs: Some(34)
            }
        ));
    }

    /// 2xx 的体只有是 JSON 对象才算一次成功的调用；其余按种类归类，诊断里只留种类与长度。
    #[test]
    fn only_a_json_object_counts_as_an_answer() {
        for ok in [
            r#"{"id":"msg_1","usage":{"input_tokens":1}}"#,
            "  \n{\"a\":1}\n",
            // 个别 Windows 上游的 JSON 带 UTF-8 BOM，之前能用，现在也要能用
            "\u{feff}{\"a\":1}",
            "\u{feff}  {\"a\":1}",
        ] {
            assert_eq!(non_json_kind(ok.as_bytes()), None, "{ok:?}");
        }
        for (body, kind) in [
            ("", "empty"),
            ("  \n\t ", "empty"),
            ("\u{feff}", "empty"),
            ("<!doctype html>\n<html lang=\"en\"></html>", "html"),
            ("  <html>", "html"),
            (r#"{"id":"msg_1""#, "malformed_json"),
            (r#"{"a":1} {"b":2}"#, "malformed_json"),
            ("OK", "other"),
            ("[1,2]", "other"),
            ("null", "other"),
            ("\"text\"", "other"),
        ] {
            assert_eq!(non_json_kind(body.as_bytes()), Some(kind), "{body:?}");
        }
    }

    /// 诊断（`error_details`）读的是 `Stream` 的原因文本：种类 + 长度，不含体的内容。
    #[test]
    fn the_diagnostic_names_the_kind_and_the_size_but_never_the_content() {
        let okapi_providers::UpstreamError::Stream(reason) = non_json_failure("html", 1166) else {
            panic!("a stream-phase failure carries the reason the diagnostics print");
        };
        assert_eq!(reason, "upstream_not_json (html, 1166 bytes)");
    }
}
