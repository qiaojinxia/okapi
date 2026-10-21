//! Bounds upstream inactivity independently of client-side SSE keep-alives.
use serde_json::Value;
use std::time::Duration;

/// SSE 响应不让前置反代缓冲：nginx 对带 `X-Accel-Buffering: no` 的响应关掉 proxy_buffering。
/// `/v1/` 的 location 本来就关了缓冲，但控制面里的 Playground 与渠道试用（同样是数据面处理器的 SSE）
/// 落在 `location /` 下，没有这个头就会被攒成整段再发——打字机效果没了，首字延迟变成整段延迟。
pub(crate) fn unbuffered(mut response: axum::response::Response) -> axum::response::Response {
    response.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    response
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamPolicy {
    pub idle: Duration,
    pub heartbeat: Duration,
}

impl StreamPolicy {
    pub fn valid(value: &Value) -> bool {
        value.as_object().is_some_and(|object| {
            object.iter().all(|(key, value)| match key.as_str() {
                "idle_timeout_secs" => value.as_u64().is_some_and(|n| (1..=480).contains(&n)),
                "heartbeat_secs" => value.as_u64().is_some_and(|n| (1..=60).contains(&n)),
                _ => false,
            })
        })
    }

    pub fn from_setting(value: Option<&Value>) -> Self {
        let value = value.filter(|v| Self::valid(v));
        Self {
            idle: Duration::from_secs(
                value
                    .and_then(|v| v["idle_timeout_secs"].as_u64())
                    .unwrap_or(120),
            ),
            heartbeat: Duration::from_secs(
                value
                    .and_then(|v| v["heartbeat_secs"].as_u64())
                    .unwrap_or(15),
            ),
        }
    }

    pub fn deadline(self, total: tokio::time::Instant) -> tokio::time::Instant {
        total.min(tokio::time::Instant::now() + self.idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_settings_are_rejected_and_legacy_values_use_defaults() {
        for value in [
            json!(null),
            json!(false),
            json!({"idle_timeout_secs":0}),
            json!({"heartbeat_secs":61}),
            json!({"idle_timeout_secs":1.5}),
            json!({"typo":5}),
        ] {
            assert!(!StreamPolicy::valid(&value));
            assert_eq!(
                StreamPolicy::from_setting(Some(&value)).idle,
                Duration::from_mins(2)
            );
        }
        let policy =
            StreamPolicy::from_setting(Some(&json!({"idle_timeout_secs":1,"heartbeat_secs":2})));
        assert_eq!(policy.idle, Duration::from_secs(1));
        assert_eq!(policy.heartbeat, Duration::from_secs(2));
        let total = tokio::time::Instant::now();
        assert_eq!(
            policy.deadline(total),
            total,
            "idle timeout cannot extend total budget"
        );
    }
}
