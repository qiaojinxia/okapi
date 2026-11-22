//! 上游回复的判定（各入口共用）：一次失败该记到 key 状态机的哪一类，2xx 的体是不是 JSON 对象。
//!
//! 原先放在 chat 里，媒体端点与图片流都要反过来引用 chat；挪出来后各入口平级引用这里。
use bytes::Bytes;
use okapi_kernel::policies::PolicyVerdict;
use okapi_providers::UpstreamError;
use okapi_providers::account_fault::AccountFault;
use okapi_store::channels::KeyFailure;

/// 这次失败该不该换渠道重试：内置的重试矩阵（[`UpstreamError::retriable_before_first_token`]），加上请求策略里点名的状态码，
/// 以及策略判成账号级故障（凭证失效 / 配额耗尽）的——问题在这把 key，不在请求，换一个就能成功。
pub fn retriable(err: &UpstreamError) -> bool {
    if err.retriable_before_first_token() {
        return true;
    }
    let UpstreamError::Status { status, body, .. } = err else {
        return false;
    };
    let policy = super::request_policy::current();
    policy.extra_retry(*status) || policy.classify(*status, body) != PolicyVerdict::None
}

/// 上游错误 → key 状态机类别（§3.6 重试矩阵）。
/// 注意顺序：insufficient_quota 判定先于 429——OpenAI 风格的
/// `429 + insufficient_quota` 语义是配额耗尽（冷却到次日），不是限速（60s）。
///
/// 请求策略（`settings.request_policy`）的状态码 / 关键词规则先于内置的表：站长点名「这个码 / 这句话就是凭证失效 / 配额耗尽」时，
/// 以他说的为准。没有策略作用域（测试、worker）时取缺省策略，也就是只有内置的表。
pub fn failure_kind_of(err: &UpstreamError) -> KeyFailure {
    if let UpstreamError::Status { status, body, .. } = err {
        match super::request_policy::current().classify(*status, body) {
            PolicyVerdict::Invalid => return KeyFailure::Invalid,
            PolicyVerdict::QuotaExhausted => return KeyFailure::QuotaExhausted,
            PolicyVerdict::None => {}
        }
    }
    match err {
        UpstreamError::Status { status, body, .. }
            if *status == 402 || (*status == 429 && body_says_insufficient_quota(body)) =>
        {
            KeyFailure::QuotaExhausted
        }
        // 429 正文明说账号配额/欠费或凭证失效（如 billing_hard_limit_reached / Arrearage /
        // BILLING_DISABLED / DeepSeek Insufficient Balance）：复用 account_fault，不能误判为 60s 限速。
        UpstreamError::Status { status: 429, .. } => match err.account_fault() {
            Some(AccountFault::QuotaExhausted) => KeyFailure::QuotaExhausted,
            Some(AccountFault::Invalid) => KeyFailure::Invalid,
            None => KeyFailure::RateLimited {
                retry_after_secs: err.retry_after_secs(),
            },
        },
        // 529（过载）：上游没给 Retry-After 时由 key_health 走指数退避（15→30→60→120→300s），
        // 不再硬编码 600s 把整池打空。
        UpstreamError::Status { status: 529, .. } => KeyFailure::RateLimited {
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
///
/// 「明说」只认上游自己写的位置：`error.code` / `error.type` / 顶层 `code` 的枚举值，`error.message` /
/// 顶层 `message` 的**句首**；不是 JSON 的短正文（代理的纯文本 403）也只看句首。正文里常回显调用方
/// 传的模型名、字段值，整段子串匹配会让任何用户用一个带 `unauthorized` 的模型名把共用 key 打成
/// 永久失效（与 `okapi_providers::account_fault` 同一条规矩）。
fn body_says_credential_rejected(body: &Bytes) -> bool {
    const CODES: [&str; 5] = [
        "invalid_api_key",
        "invalid_authentication",
        "authentication_error",
        "unauthorized",
        "invalid_token",
    ];
    const PREFIXES: [&str; 6] = [
        "invalid api key",
        "incorrect api key",
        "invalid authentication",
        "invalid token",
        "api key not valid",
        "unauthorized",
    ];
    let starts_with = |text: &str| {
        let text = text.trim_start();
        PREFIXES.iter().any(|prefix| {
            text.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
    };
    if body.len() > 16 * 1024 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        // 纯文本 / 网页：只认句首（`Unauthorized`、`Invalid token`）
        return starts_with(&String::from_utf8_lossy(body));
    };
    let error = value.get("error");
    let codes = [
        error.and_then(|e| e.get("code")),
        error.and_then(|e| e.get("type")),
        value.get("code"),
    ];
    let messages = [
        error.and_then(|e| e.get("message")),
        value.get("message"),
        // `{"error": "Unauthorized"}` 这种平铺字符串
        error.filter(|e| e.is_string()),
    ];
    codes
        .iter()
        .flatten()
        .filter_map(|code| code.as_str())
        .any(|code| CODES.iter().any(|known| code.eq_ignore_ascii_case(known)))
        || messages
            .iter()
            .flatten()
            .filter_map(|message| message.as_str())
            .any(starts_with)
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
pub fn non_json_kind(body: &[u8]) -> Option<&'static str> {
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
pub fn non_json_failure(kind: &str, len: usize) -> UpstreamError {
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

    /// 403 正文里回显的调用方内容（模型名、字段值）不能把共用 key 打成永久失效：只认上游自己写的位置。
    #[test]
    fn a_403_echoing_caller_input_does_not_invalidate_the_key() {
        for echoed in [
            r#"{"error":{"code":"model_not_allowed","message":"model 'gpt-unauthorized' is not enabled for this token group"}}"#,
            r#"{"error":{"message":"No access to model invalid token-x"},"type":"error"}"#,
            r#"{"message":"Model invalid_api_key-mini is not available in your plan"}"#,
            "access denied for model unauthorized-preview",
        ] {
            assert!(
                matches!(failure_kind_of(&status(403, echoed)), KeyFailure::Request),
                "{echoed}"
            );
        }
        for rejected in [
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            r#"{"error":{"message":"Invalid token (request id: 2026)","type":"new_api_error"}}"#,
            r#"{"error":"Unauthorized"}"#,
            r#"{"code":"UNAUTHORIZED","message":"denied"}"#,
            "Unauthorized",
            "  invalid api key provided",
        ] {
            assert!(
                matches!(failure_kind_of(&status(403, rejected)), KeyFailure::Invalid),
                "{rejected}"
            );
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
            failure_kind_of(&status(
                429,
                r#"{"error":{"code":"billing_hard_limit_reached","message":"limit"}}"#
            )),
            KeyFailure::QuotaExhausted
        ));
        assert!(matches!(
            failure_kind_of(&status(
                429,
                r#"{"error":{"message":"Your credit balance is too low"}}"#
            )),
            KeyFailure::QuotaExhausted
        ));
        // 529 无 Retry-After 时交给 key_health 走 Redis 指数退避（不再硬编码 600s）
        assert!(matches!(
            failure_kind_of(&status(529, "")),
            KeyFailure::RateLimited {
                retry_after_secs: None
            }
        ));
        // 模型/区域级 404/400 允许首字前换渠道且不伤整把 key
        let model_404 = status(
            404,
            r#"{"error":{"code":"model_not_found","message":"no such model"}}"#,
        );
        assert!(retriable(&model_404));
        assert!(matches!(failure_kind_of(&model_404), KeyFailure::Request));
        let region_400 = status(
            400,
            r#"{"error":{"code":400,"message":"User location is not supported for the API use.","status":"FAILED_PRECONDITION"}}"#,
        );
        assert!(retriable(&region_400));
        assert!(matches!(failure_kind_of(&region_400), KeyFailure::Request));
        assert!(matches!(
            failure_kind_of(&status(503, "")),
            KeyFailure::Transient
        ));
    }

    /// 请求策略是**追加**规则：作用域外只有内置的表；作用域内站长点名的状态码 / 关键词先认，内置的照常生效。
    #[tokio::test]
    async fn the_request_policy_adds_to_the_builtin_tables_and_only_inside_its_scope() {
        let policy = std::sync::Arc::new(
            okapi_kernel::policies::RequestPolicy::parse(&serde_json::json!({
                "retry_statuses": ["404"],
                "invalid_statuses": ["418"],
                "invalid_keywords": ["zxq-ban-notice"],
                "quota_keywords": ["zxq-wallet-empty"],
            }))
            .unwrap(),
        );
        assert!(matches!(
            failure_kind_of(&status(418, "")),
            KeyFailure::Request
        ));
        assert!(!retriable(&status(404, "")));
        crate::request_policy::with(policy, async {
            assert!(matches!(
                failure_kind_of(&status(418, "")),
                KeyFailure::Invalid
            ));
            assert!(matches!(
                failure_kind_of(&status(
                    400,
                    r#"{"error":{"message":"ZXQ-Ban-Notice: call us"}}"#
                )),
                KeyFailure::Invalid
            ));
            assert!(matches!(
                failure_kind_of(&status(500, "zxq-wallet-empty, top up")),
                KeyFailure::QuotaExhausted
            ));
            // 内置的表照常生效
            assert!(matches!(
                failure_kind_of(&status(401, "")),
                KeyFailure::Invalid
            ));
            assert!(matches!(
                failure_kind_of(&status(503, "")),
                KeyFailure::Transient
            ));
            assert!(retriable(&status(404, "")));
            assert!(retriable(&status(500, "")));
            assert!(
                retriable(&status(400, "zxq-wallet-empty")),
                "a fault the policy calls account-level is retried on another channel"
            );
            assert!(!retriable(&status(400, "plain bad request")));
        })
        .await;
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
