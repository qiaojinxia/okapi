//! 不产生账本预扣的计数准入。独立 RPM/RPD 窗口与带 TTL 的请求租约；Redis 故障拒绝。
use super::SchedulerRedis;
use crate::gateway::error::AppError;
use axum::http::StatusCode;
use fred::interfaces::{LuaInterface, SortedSetsInterface};
use okapi_api::codes;
use okapi_store::{AuthedKey, ChannelCandidate};
use uuid::Uuid;

pub(crate) struct CountPermit {
    sched: SchedulerRedis,
    lease: Option<(String, String)>,
}

impl CountPermit {
    pub async fn acquire(sched: &SchedulerRedis, key: &AuthedKey) -> Result<Self, AppError> {
        const LUA: &str = r"
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
            if tonumber(redis.call('GET', KEYS[2]) or '0') >= tonumber(ARGV[3]) then return 1 end
            if tonumber(ARGV[4]) > 0 and tonumber(redis.call('GET', KEYS[3]) or '0') >= tonumber(ARGV[4]) then return 2 end
            if redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[5]) then return 3 end
            redis.call('INCR', KEYS[2]); redis.call('EXPIRE', KEYS[2], 120)
            redis.call('INCR', KEYS[3]); redis.call('EXPIRE', KEYS[3], 172800)
            redis.call('ZADD', KEYS[1], tonumber(ARGV[1]) + 90000, ARGV[2])
            redis.call('EXPIRE', KEYS[1], 120)
            return 0
        ";
        let now = chrono::Utc::now().timestamp_millis();
        let prefix = format!("count:{{{}}}", key.key_id);
        let lease = format!("{prefix}:leases");
        let request = Uuid::new_v4().to_string();
        let rpm = key.rpm_limit.filter(|n| *n > 0).unwrap_or(60);
        let rpd = key.rpd_limit.filter(|n| *n > 0).unwrap_or(0);
        let concurrency = key.max_concurrency.filter(|n| *n > 0).unwrap_or(4);
        let outcome: i64 = sched
            .client
            .eval(
                LUA,
                vec![
                    lease.clone(),
                    format!("{prefix}:rpm:{}", now / 60_000),
                    format!("{prefix}:rpd:{}", now / 86_400_000),
                ],
                vec![
                    now.to_string(),
                    request.clone(),
                    rpm.to_string(),
                    rpd.to_string(),
                    concurrency.to_string(),
                ],
            )
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, "token count admission unavailable");
                AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::OVERLOADED)
            })?;
        let param = match outcome {
            0 => {
                return Ok(Self {
                    sched: sched.clone(),
                    lease: Some((lease, request)),
                });
            }
            1 => "token_count_rpm",
            2 => "token_count_rpd",
            _ => "token_count_concurrency",
        };
        Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(param))
    }

    // 释放先移交独立任务：HTTP handler 取消时也完成清理；ZREM 可安全重试。
    pub async fn release(mut self) {
        if let Some(task) = self.spawn_release() {
            let _ = task.await;
        }
    }

    fn spawn_release(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let (key, request) = self.lease.take()?;
        let sched = self.sched.clone();
        Some(runtime.spawn(async move {
            let _: Result<i64, _> = sched.client.zrem(key, request).await;
        }))
    }
}

impl Drop for CountPermit {
    fn drop(&mut self) {
        self.spawn_release();
    }
}

/// 与生成请求共享渠道并发槽，任何返回/取消路径恰好归还一次。
pub(crate) struct ChannelPermit {
    sched: SchedulerRedis,
    slot: Option<(i64, Option<i32>)>,
}

impl ChannelPermit {
    pub async fn acquire(sched: &SchedulerRedis, cand: &ChannelCandidate) -> Option<Self> {
        sched
            .acquire_slot(cand.channel_key_id, cand.max_concurrency)
            .await
            .then(|| Self {
                sched: sched.clone(),
                slot: Some((cand.channel_key_id, cand.max_concurrency)),
            })
    }

    pub async fn release(mut self) {
        if let Some(task) = self.spawn_release() {
            let _ = task.await;
        }
    }

    fn spawn_release(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let (key, cap) = self.slot.take()?;
        let sched = self.sched.clone();
        Some(runtime.spawn(async move {
            sched.release_slot(key, cap).await;
        }))
    }
}

impl Drop for ChannelPermit {
    fn drop(&mut self) {
        self.spawn_release();
    }
}
