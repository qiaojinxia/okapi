//! 媒体端点的绑定与租约：视频任务 → 渠道 key、Realtime 每 key 连接租约。
use super::SchedulerRedis;
use fred::interfaces::{KeysInterface, LuaInterface, SortedSetsInterface};
use fred::types::Expiration;

impl SchedulerRedis {
    /// videos 任务 → 渠道 key 映射写入（48h；键含 user_id 天然租户隔离）。
    pub async fn video_task_set(&self, user_id: i64, task_id: &str, channel_key_id: i64) {
        let key = format!("video:task:{{{user_id}}}:{task_id}");
        let _: Result<(), _> = self
            .client
            .set(
                &key,
                channel_key_id.to_string(),
                Some(Expiration::EX(48 * 3600)),
                None,
                false,
            )
            .await;
    }

    /// videos 任务映射读取（None = 未知任务/已过期/非本用户）。
    pub async fn video_task_get(&self, user_id: i64, task_id: &str) -> Option<i64> {
        let key = format!("video:task:{{{user_id}}}:{task_id}");
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok())
    }

    fn ws_lease_key(key_id: i64) -> String {
        format!("ws:lease:k:{key_id}")
    }

    /// Realtime WS per-key 连接租约获取（§14.4；docs/database.md §2.1 ws:lease:k:*）。
    /// ZSET 成员 = 连接 id，score = 租约到期毫秒：先清过期再计数，原子准入。
    /// 崩溃的连接不续期即自然滚出窗口，无泄漏。Redis 故障放行（与其余限流一致）。
    /// 到期时间按 Redis 服务器时钟算（同 `channel_permit`）：各副本本机时钟有偏差时，
    /// 快的那台会把别的副本刚续上的存活租约当成过期清掉，放进超额连接。
    pub async fn ws_lease_acquire(&self, key_id: i64, conn_id: &str, limit: i64) -> bool {
        const LUA: &str = r"
            local t = redis.call('TIME')
            local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now)
            if redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[1]) then return 0 end
            redis.call('ZADD', KEYS[1], now + 60000, ARGV[2])
            redis.call('PEXPIRE', KEYS[1], 6 * 3600 * 1000)
            return 1
        ";
        let result: Result<i64, _> = self
            .client
            .eval(
                LUA,
                vec![Self::ws_lease_key(key_id)],
                vec![limit.to_string(), conn_id.to_owned()],
            )
            .await;
        result.map_or(true, |v| v == 1)
    }

    /// 租约续期（会话泵内每 20s；60s 窗口容忍两次丢失）。与获取同用 Redis 服务器时钟。
    /// 续租；返回 false = 租约已不在（过期被别的连接清掉、或已释放），会话应当结束。
    /// Redis 不可达时照旧放行：与 `ws_lease_acquire` 同为 fail-open，Redis 抖一下不该掐断所有会话。
    pub async fn ws_lease_renew(&self, key_id: i64, conn_id: &str) -> bool {
        const LUA: &str = r"
            local t = redis.call('TIME')
            local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
            local old = redis.call('ZSCORE', KEYS[1], ARGV[1])
            if not old or tonumber(old) <= now then return 0 end
            redis.call('ZADD', KEYS[1], now + 60000, ARGV[1])
            return 1
        ";
        self.client
            .eval::<i64, _, _, _>(
                LUA,
                vec![Self::ws_lease_key(key_id)],
                vec![conn_id.to_owned()],
            )
            .await
            .map_or(true, |renewed| renewed == 1)
    }

    /// 连接断开释放租约（断开/失败路径统一走这里）。
    pub async fn ws_lease_release(&self, key_id: i64, conn_id: &str) {
        let _: Result<i64, _> = self.client.zrem(Self::ws_lease_key(key_id), conn_id).await;
    }
}
