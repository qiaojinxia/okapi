//! Responses WS uses a separate, fail-closed connection lease.
use super::SchedulerRedis;
use crate::gateway::error::AppError;
use axum::http::StatusCode;
use fred::interfaces::{LuaInterface, SortedSetsInterface};
use okapi_api::codes;
use std::time::Duration;

fn key(id: i64) -> String {
    format!("ws:responses:k:{id}")
}

fn unavailable() -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::INTERNAL_ERROR)
        .with_param("responses_ws_lease")
}

impl SchedulerRedis {
    pub(crate) async fn responses_ws_acquire(
        &self,
        id: i64,
        connection: &str,
        limit: i64,
    ) -> Result<bool, AppError> {
        self.responses_ws_lease(id, connection, limit, false).await
    }

    pub(crate) async fn responses_ws_renew(
        &self,
        id: i64,
        connection: &str,
    ) -> Result<bool, AppError> {
        self.responses_ws_lease(id, connection, 0, true).await
    }

    async fn responses_ws_lease(
        &self,
        id: i64,
        connection: &str,
        limit: i64,
        renew: bool,
    ) -> Result<bool, AppError> {
        // 时间取 Redis 服务器时钟（同 ws_lease_acquire）：各副本本机时钟有偏差时，快的那台会把
        // 别的副本刚续上的存活租约当成过期清掉，放进超额连接
        let script = if renew {
            "local t=redis.call('TIME'); local now=tonumber(t[1])*1000+math.floor(tonumber(t[2])/1000); local old=redis.call('ZSCORE',KEYS[1],ARGV[3]); if not old or tonumber(old)<=now then return 0 end; redis.call('ZADD',KEYS[1],now+60000,ARGV[3]); redis.call('PEXPIRE',KEYS[1],120000); return 1"
        } else {
            "local t=redis.call('TIME'); local now=tonumber(t[1])*1000+math.floor(tonumber(t[2])/1000); redis.call('ZREMRANGEBYSCORE',KEYS[1],'-inf',now); if redis.call('ZCARD',KEYS[1])>=tonumber(ARGV[2]) then return 0 end; redis.call('ZADD',KEYS[1],now+60000,ARGV[3]); redis.call('PEXPIRE',KEYS[1],120000); return 1"
        };
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            self.client.eval::<i64, _, _, _>(
                script,
                vec![key(id)],
                // ARGV[1] 保留占位，脚本不再读它
                vec!["0".to_owned(), limit.to_string(), connection.to_owned()],
            ),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        Ok(result == 1)
    }

    pub(crate) async fn responses_ws_release(&self, id: i64, connection: &str) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.client.zrem::<i64, _, _>(key(id), connection),
        )
        .await;
    }
}
