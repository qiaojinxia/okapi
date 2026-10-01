//! Bounds upstream inactivity independently of client-side SSE keep-alives.
use serde_json::Value;
use std::time::Duration;

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
