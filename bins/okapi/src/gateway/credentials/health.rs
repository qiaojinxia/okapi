//! Non-secret refresh observations; failure backoff is shared by worker replicas.
use super::super::state::AppState;
use fred::interfaces::KeysInterface;
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

async fn read_raw(state: &AppState, key: i64) -> Result<Option<String>, fred::error::Error> {
    state
        .sched
        .client()
        .get(format!("oauth:refresh:{key}"))
        .await
}

pub async fn read(state: &AppState, key: i64) -> Option<RefreshHealth> {
    let payload = read_raw(state, key).await.ok()?;
    serde_json::from_str(payload.as_deref()?).ok()
}

/// gateway 与 worker 会并发记同一把 key：读出原文、在本地算新值，只在原文没被别人改过时写回，
/// 被抢先就重读再算。裸 GET→SET 会让并发的两次失败只记成一次，退避提前结束。
pub async fn record(state: &AppState, key: i64, failure: Option<&str>) {
    use fred::interfaces::LuaInterface;
    for _ in 0..8 {
        let raw = match read_raw(state, key).await {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(key, error = %error, "OAuth refresh observation unavailable");
                return;
            }
        };
        let mut health: RefreshHealth = raw
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok())
            .unwrap_or_default();
        let now = chrono::Utc::now().timestamp();
        match failure {
            Some(code) => health.failed(now, code),
            None => health.succeeded(now),
        }
        let Ok(payload) = serde_json::to_string(&health) else {
            return;
        };
        let written: Result<i64, _> = state
            .sched
            .client()
            .eval(
                r"
            local old=redis.call('GET',KEYS[1]) or ''
            if old~=ARGV[1] then return 0 end
            redis.call('SET',KEYS[1],ARGV[2],'EX',ARGV[3]); return 1
        ",
                vec![format!("oauth:refresh:{key}")],
                vec![
                    raw.unwrap_or_default(),
                    payload,
                    (30 * 24 * 3600).to_string(),
                ],
            )
            .await;
        match written {
            Ok(1) => return,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(key, error = %error, "OAuth refresh observation unavailable");
                return;
            }
        }
    }
    tracing::warn!(key, "OAuth refresh observation lost to concurrent writers");
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
