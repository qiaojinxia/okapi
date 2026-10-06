//! Non-secret refresh observations; failure backoff is shared by worker replicas.
use super::super::state::AppState;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RefreshHealth {
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub consecutive_failures: u32,
    pub next_retry_at: Option<i64>,
    pub error_code: Option<String>,
}

impl RefreshHealth {
    pub fn retry_ready(&self, now: i64) -> bool {
        self.next_retry_at.is_none_or(|next| now >= next)
    }

    fn succeeded(&mut self, now: i64) {
        self.last_attempt_at = Some(now);
        self.last_success_at = Some(now);
        self.consecutive_failures = 0;
        self.next_retry_at = None;
        self.error_code = None;
    }

    fn failed(&mut self, now: i64, code: &str) {
        self.last_attempt_at = Some(now);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let shift = self.consecutive_failures.saturating_sub(1).min(5);
        self.next_retry_at = Some(now.saturating_add((30_i64 << shift).min(900)));
        self.error_code = Some(code.to_owned());
    }
}

pub async fn read(state: &AppState, key: i64) -> Option<RefreshHealth> {
    let payload: Option<String> = state
        .sched
        .client()
        .get(format!("oauth:refresh:{key}"))
        .await
        .ok()?;
    serde_json::from_str(payload.as_deref()?).ok()
}

pub async fn record(state: &AppState, key: i64, failure: Option<&str>) {
    let mut health = read(state, key).await.unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    match failure {
        Some(code) => health.failed(now, code),
        None => health.succeeded(now),
    }
    let Ok(payload) = serde_json::to_string(&health) else {
        return;
    };
    let result: Result<(), _> = state
        .sched
        .client()
        .set(
            format!("oauth:refresh:{key}"),
            payload,
            Some(Expiration::EX(30 * 24 * 3600)),
            None,
            false,
        )
        .await;
    if let Err(error) = result {
        tracing::warn!(key, error = %error, "OAuth refresh observation unavailable");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_back_off_without_losing_last_success_and_recovery_resets() {
        let mut health = RefreshHealth::default();
        health.succeeded(10);
        for (n, delay) in [30, 60, 120, 240, 480, 900, 900].into_iter().enumerate() {
            health.failed(100, "upstream_timeout");
            assert_eq!(health.consecutive_failures, u32::try_from(n).unwrap() + 1);
            assert_eq!(health.next_retry_at, Some(100 + delay));
            assert_eq!(health.last_success_at, Some(10));
            assert!(!health.retry_ready(99 + delay));
            assert!(health.retry_ready(100 + delay));
        }
        health.succeeded(200);
        assert_eq!(health.consecutive_failures, 0);
        assert!(health.error_code.is_none());
        assert!(health.retry_ready(200));
    }
}
