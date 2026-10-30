//! 首字前到达的流内失败事件：上游先回了 200 和一串开场事件，才在流里说"我失败了"。
//!
//! Anthropic 适配器已把流内 `error` 事件翻成带等价 HTTP 状态的 [`UpstreamError`]
//! （`anthropic::stream_error`）。Responses 与 OpenAI chat 的流没有这一步：`{"type":"error",…}`、
//! `response.failed` 在首字前到达时，网关只看到一条没有产出的流，记成 `empty_completion`——上游说的原因
//! （2026-10-29 线上：ChatGPT 后端的 `server_is_overloaded`）丢了，重试矩阵与 key 冷却也按"空回复"
//! 而不是"过载 / 限流"走。这里只做识别与分类；是否在首字前由调用方（网关的首字缓冲）判断。

use crate::error::UpstreamError;
use bytes::Bytes;
use serde_json::{Value, json};

/// 说明截到这么长再写进诊断（上游偶尔把整段请求回显进来）。
const MAX_MESSAGE_CHARS: usize = 300;

/// 一条流内失败事件里的错误对象。
#[derive(Debug, Clone)]
pub struct StreamFailure {
    error: Value,
}

/// `raw` 是不是一条流内失败事件：Responses 的 `error` / `response.failed`，或 chat 流里没有 `type`
/// 的块带着顶层 `error` 对象。别的事件即使正文里提到 error 也不算。
#[must_use]
pub fn detect(raw: &str) -> Option<StreamFailure> {
    // 绝大多数事件是正常增量：先用子串排除，省一次 JSON 解析
    if !raw.contains("error") && !raw.contains("response.failed") {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let error = match value.get("type").and_then(Value::as_str) {
        Some("response.failed") => value
            .pointer("/response/error")
            .filter(|error| error.is_object())
            .cloned()
            .unwrap_or_else(|| json!({"code": "response_failed"})),
        Some("error") => match value.get("error") {
            Some(error) if error.is_object() => error.clone(),
            // 平铺形：{"type":"error","code":…,"message":…,"param":…}
            _ => json!({
                "code": value.get("code"),
                "message": value.get("message"),
                "param": value.get("param"),
            }),
        },
        Some(_) => return None,
        None => value
            .get("error")
            .filter(|error| error.is_object())?
            .clone(),
    };
    Some(StreamFailure { error })
}

impl StreamFailure {
    fn field(&self, name: &str) -> Option<&str> {
        self.error
            .get(name)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    }

    /// 错误码（`code`），没有就退到 `type`。
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.field("code").or_else(|| self.field("type"))
    }

    /// 上游给的说明（截断）；没有说明就用错误码。
    #[must_use]
    pub fn message(&self) -> String {
        self.field("message")
            .or_else(|| self.code())
            .unwrap_or("stream_failure")
            .chars()
            .take(MAX_MESSAGE_CHARS)
            .collect()
    }

    /// 认得出的类型对应的 HTTP 等价状态（`code` 优先，再看 `type`）。
    ///
    /// 只收确认过含义的名字：状态会驱动 key 状态机（429 冷却、5xx 计失败），认错了比不认更糟，
    /// 认不出的返回 None，由调用方照旧当空流处理。401 / 403 一律不在表里——流里的一句话不足以
    /// 把一把 key 判成失效。
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        [self.field("code"), self.field("type")]
            .into_iter()
            .flatten()
            .find_map(status_of)
    }

    /// 认得出的类型 → 带等价状态的上游错误：体是 OpenAI 错误信封 `{"error":{…}}`，首字前失败且不可
    /// 重试时客户端拿到的就是它。认不出的原样交还。
    ///
    /// # Errors
    /// 认不出类型时返回自身。
    pub fn into_upstream_error(self) -> Result<UpstreamError, Self> {
        let Some(status) = self.status() else {
            return Err(self);
        };
        let retry_after_secs = if status == 429 {
            self.retry_after_secs()
        } else {
            None
        };
        Ok(UpstreamError::Status {
            status,
            body: Bytes::from(json!({ "error": self.error }).to_string()),
            retry_after_secs,
        })
    }

    /// 订阅窗口用尽时 ChatGPT 后端随错误给的恢复时间：`resets_in_seconds`（相对）或 `resets_at`（Unix 秒）。
    fn retry_after_secs(&self) -> Option<i64> {
        if let Some(secs) = self
            .error
            .get("resets_in_seconds")
            .and_then(Value::as_i64)
            .filter(|secs| *secs > 0)
        {
            return Some(secs);
        }
        let wait =
            self.error.get("resets_at").and_then(Value::as_i64)? - chrono::Utc::now().timestamp();
        (wait > 0).then_some(wait)
    }
}

fn status_of(name: &str) -> Option<u16> {
    match name {
        // 上游过载 / 暂不可用：换个时间或换个渠道就行
        "server_is_overloaded"
        | "slow_down"
        | "overloaded_error"
        | "service_unavailable_error"
        | "service_unavailable" => Some(503),
        // 限速 / 订阅窗口用尽 / 配额耗尽：按 429 走 key 冷却（insufficient_quota 由网关按体判成配额耗尽）
        "rate_limit_exceeded"
        | "rate_limit_error"
        | "usage_limit_reached"
        | "insufficient_quota" => Some(429),
        "server_error" | "internal_server_error" | "api_error" => Some(500),
        // 请求本身的问题：换渠道也一样，原样回给客户端
        "context_length_exceeded" | "invalid_prompt" => Some(400),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_and_code(raw: &str) -> Option<(Option<u16>, String)> {
        detect(raw).map(|failure| (failure.status(), failure.message()))
    }

    #[test]
    fn overloaded_error_event_from_the_chatgpt_backend_is_a_503() {
        // 2026-10-29 线上原文（渠道测试按钮审计里留下的 upstream_body）
        let raw = r#"{"type":"error","sequence_number":3,"error":{"message":"Our servers are currently overloaded. Please try again later.","type":"service_unavailable_error","param":null,"code":"server_is_overloaded"}}"#;
        let failure = detect(raw).expect("流内 error 事件");
        assert_eq!(failure.code(), Some("server_is_overloaded"));
        assert_eq!(failure.status(), Some(503));
        assert_eq!(
            failure.message(),
            "Our servers are currently overloaded. Please try again later."
        );
        let Ok(UpstreamError::Status {
            status,
            body,
            retry_after_secs,
        }) = failure.into_upstream_error()
        else {
            panic!("认得出的类型要变成带状态的上游错误");
        };
        assert_eq!((status, retry_after_secs), (503, None));
        let envelope: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope["error"]["code"], "server_is_overloaded");
        assert!(envelope.get("type").is_none(), "体是错误信封，不带事件外壳");
    }

    #[test]
    fn response_failed_and_flat_error_events_are_recognised() {
        let failed = r#"{"type":"response.failed","response":{"id":"resp_1","status":"failed","error":{"code":"rate_limit_exceeded","message":"Rate limit reached"}}}"#;
        assert_eq!(
            status_and_code(failed),
            Some((Some(429), "Rate limit reached".to_owned()))
        );
        // OpenAI 文档里的平铺形
        let flat = r#"{"type":"error","code":"context_length_exceeded","message":"too long","param":null,"sequence_number":1}"#;
        assert_eq!(
            status_and_code(flat),
            Some((Some(400), "too long".to_owned()))
        );
        // 没有错误对象的 response.failed 仍是失败，只是认不出原因
        assert_eq!(
            status_and_code(
                r#"{"type":"response.failed","response":{"status":"failed","error":null}}"#
            ),
            Some((None, "response_failed".to_owned()))
        );
        // chat 流里没有 type 的块带顶层 error 对象
        assert_eq!(
            status_and_code(
                r#"{"error":{"message":"The server had an error","type":"server_error"}}"#
            ),
            Some((Some(500), "The server had an error".to_owned()))
        );
    }

    #[test]
    fn ordinary_events_are_never_failures() {
        for raw in [
            r#"{"type":"response.created","response":{"id":"resp_1","error":null}}"#,
            r#"{"type":"response.output_text.delta","delta":"an error occurred in your code"}"#,
            r#"{"type":"response.completed","response":{"error":null}}"#,
            r#"{"choices":[{"delta":{"content":"error"}}],"error":null}"#,
            r#"{"id":"chatcmpl-1","error":"string errors are not stream failures"}"#,
            "not json, but mentions error",
            "",
        ] {
            assert!(detect(raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn unknown_codes_stay_unclassified_and_credentials_are_never_inferred() {
        for (code, kind) in [
            ("some_new_code", "invalid_request_error"),
            ("invalid_api_key", "authentication_error"),
            ("permission_denied", "permission_error"),
            ("model_not_found", "not_found_error"),
        ] {
            let raw =
                json!({"type":"error","error":{"code":code,"type":kind,"message":"m"}}).to_string();
            let failure = detect(&raw).unwrap();
            assert_eq!(failure.status(), None, "{code}");
            let Err(back) = failure.into_upstream_error() else {
                panic!("{code} 不该被分类");
            };
            assert_eq!(back.message(), "m");
        }
    }

    #[test]
    fn usage_limit_carries_its_recovery_time() {
        let raw = r#"{"type":"error","error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_in_seconds":5400}}"#;
        let Ok(UpstreamError::Status {
            status,
            retry_after_secs,
            ..
        }) = detect(raw).unwrap().into_upstream_error()
        else {
            panic!("usage_limit_reached 是 429");
        };
        assert_eq!((status, retry_after_secs), (429, Some(5400)));

        let at = chrono::Utc::now().timestamp() + 3600;
        let raw = json!({"type":"error","error":{"code":"usage_limit_reached","resets_at":at}})
            .to_string();
        let Ok(UpstreamError::Status {
            retry_after_secs, ..
        }) = detect(&raw).unwrap().into_upstream_error()
        else {
            panic!("usage_limit_reached 是 429");
        };
        assert!(retry_after_secs.is_some_and(|secs| (3590..=3600).contains(&secs)));

        // 已过去的重置时刻不能变成负数 / 零冷却
        let raw = json!({"type":"error","error":{"code":"usage_limit_reached","resets_at":1}})
            .to_string();
        let Ok(UpstreamError::Status {
            retry_after_secs, ..
        }) = detect(&raw).unwrap().into_upstream_error()
        else {
            panic!("usage_limit_reached 是 429");
        };
        assert_eq!(retry_after_secs, None);
    }

    #[test]
    fn long_messages_are_bounded() {
        let raw =
            json!({"type":"error","error":{"code":"server_error","message":"中".repeat(2000)}})
                .to_string();
        assert_eq!(
            detect(&raw).unwrap().message().chars().count(),
            MAX_MESSAGE_CHARS
        );
    }
}
