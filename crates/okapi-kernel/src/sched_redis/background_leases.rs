//! 跨副本的后台租约：OAuth 凭证刷新锁、出口探测轮次、维护任务互斥与轮次占位、跨副本健康脏标记。
use super::SchedulerRedis;
use fred::interfaces::{KeysInterface, SetsInterface};
use fred::types::{Expiration, SetOptions};
use okapi_store::CachedScript;
use std::collections::HashSet;

/// 维护任务跨副本认领结果（执行互斥锁 + 周期轮次占位）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintenanceLease {
    /// 本副本拿到互斥锁（带唯一 token，运行中可续租，结束或停机时按 CAS 释放）。
    Acquired(String),
    /// 另一副本正在执行或本周期已被认领，本次跳过。
    Skipped,
    /// Redis 暂不可达：放行（维护任务底层幂等，不因 Redis 瞬断停摆）。
    RedisUnavailable,
}

const DIRTY_KEY_SET: &str = "health:dirty:ck";
const DIRTY_EGRESS_SET: &str = "health:dirty:egress";
const DIRTY_SET_TTL_SECS: i64 = 24 * 3600;

/// 维护任务 Redis 键：使用 `{job}` hash tag 保证 Redis Cluster 下同槽，可供双键 Lua 脚本原子执行。
fn maint_lock_key(job: &str) -> String {
    format!("lock:maint:{{{job}}}")
}

fn maint_round_key(job: &str) -> String {
    format!("maint:round:{{{job}}}")
}

impl SchedulerRedis {
    /// 刷新租约的时长（秒）。临界区里每一步都有上限（见 `credentials::oauth`），总长压得进它。
    const CRED_LOCK_TTL_SECS: i64 = 90;

    /// Refresh lease shared by request, worker and manual refresh. Redis failure is fail-closed.
    pub async fn cred_lock_acquire(
        &self,
        channel_key_id: i64,
    ) -> Result<Option<String>, fred::error::Error> {
        let owner = uuid::Uuid::new_v4().to_string();
        let set: Result<Option<String>, _> = self
            .client
            .set(
                format!("lock:cred:{channel_key_id}"),
                owner.clone(),
                Some(Expiration::EX(Self::CRED_LOCK_TTL_SECS)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        set.map(|result| result.map(|_| owner))
    }

    /// 续租：锁仍归 `owner` 才把剩余时间拉回满额；返回是否续上。刷新成功后立刻续一次，
    /// 让"旧 refresh token 已作废 → 新凭据落盘"这一段有整段租约可用。
    pub async fn cred_lock_extend(&self, channel_key_id: i64, owner: &str) -> bool {
        static SCRIPT: CachedScript = CachedScript::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('EXPIRE', KEYS[1], ARGV[2]) else return 0 end",
        );
        let renewed: Result<i64, _> = SCRIPT
            .eval(
                &self.client,
                vec![format!("lock:cred:{channel_key_id}")],
                vec![owner.to_owned(), Self::CRED_LOCK_TTL_SECS.to_string()],
            )
            .await;
        matches!(renewed, Ok(1))
    }

    pub async fn cred_lock_release(&self, channel_key_id: i64, owner: &str) {
        static SCRIPT: CachedScript = CachedScript::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end",
        );
        let _: Result<i64, _> = SCRIPT
            .eval(
                &self.client,
                vec![format!("lock:cred:{channel_key_id}")],
                vec![owner],
            )
            .await;
    }

    /// 出口代理后台探测的本轮租约（§11.41）：SET NX EX，拿到的副本执行这一轮。
    /// Redis 不可用时放行（最坏多个副本重复探一轮），不能让告警因为 Redis 抖一下就静默。
    pub async fn egress_probe_claim(&self, holder: &str, ttl_secs: i64) -> bool {
        let claimed: Result<Option<String>, _> = self
            .client
            .set(
                "egress:probe:round",
                holder,
                Some(Expiration::EX(ttl_secs.max(1))),
                Some(SetOptions::NX),
                false,
            )
            .await;
        !matches!(claimed, Ok(None))
    }

    /// 渠道 key 后台探测的占位（`channel_probe`）：同一把 key 在 `ttl_secs` 内只有一个副本去探（SET NX EX）。
    /// 占位本身就是「最近探过」的标记，探测间隔由它的 TTL 决定。Redis 不可用时放行——最坏重复探一次，
    /// 不能因为 Redis 抖一下让恢复停摆。
    pub async fn channel_probe_claim(
        &self,
        channel_key_id: i64,
        holder: &str,
        ttl_secs: i64,
    ) -> bool {
        let claimed: Result<Option<String>, _> = self
            .client
            .set(
                format!("probe:ck:{channel_key_id}"),
                holder,
                Some(Expiration::EX(ttl_secs.max(1))),
                Some(SetOptions::NX),
                false,
            )
            .await;
        !matches!(claimed, Ok(None))
    }

    /// 渠道上游余额刷新的占位（`channel_balance`）：同一个渠道在 `ttl_secs` 内只有一个副本去查（SET NX EX），
    /// 占位本身就是「最近查过」的标记。Redis 不可用时放行——最坏重复查一次余额接口。
    pub async fn channel_balance_claim(
        &self,
        channel_id: i64,
        holder: &str,
        ttl_secs: i64,
    ) -> bool {
        let claimed: Result<Option<String>, _> = self
            .client
            .set(
                format!("balance:ch:{channel_id}"),
                holder,
                Some(Expiration::EX(ttl_secs.max(1))),
                Some(SetOptions::NX),
                false,
            )
            .await;
        !matches!(claimed, Ok(None))
    }

    /// 维护任务跨副本认领：原子检查「活跃执行锁」与「轮次占位」均不存在后一并写入。
    ///
    /// - `lock_ttl_secs`：执行互斥锁 TTL，运行期间由心跳续租，结束/停机时按 CAS 主动释放；
    /// - `round_ttl_secs`：周期占位 TTL（`<= 0` 表示不写轮次占位），执行结束后不删除，
    ///   避免任务 300ms 跑完后另一副本 2 秒后重复扫描。
    pub async fn maintenance_claim(
        &self,
        job: &str,
        holder: &str,
        lock_ttl_secs: i64,
        round_ttl_secs: i64,
    ) -> MaintenanceLease {
        static SCRIPT: CachedScript = CachedScript::new(
            "if redis.call('EXISTS', KEYS[1]) == 1 then return 0 end \
             if tonumber(ARGV[3]) > 0 and redis.call('EXISTS', KEYS[2]) == 1 then return 0 end \
             redis.call('SET', KEYS[1], ARGV[1], 'EX', tonumber(ARGV[2])) \
             if tonumber(ARGV[3]) > 0 then redis.call('SET', KEYS[2], ARGV[1], 'EX', tonumber(ARGV[3])) end \
             return 1",
        );
        let token = format!("{holder}:{}", uuid::Uuid::new_v4());
        let lock_ttl = lock_ttl_secs.max(1);
        let round_ttl = round_ttl_secs.max(0);
        let result: Result<i64, _> = SCRIPT
            .eval(
                &self.client,
                vec![maint_lock_key(job), maint_round_key(job)],
                vec![token.clone(), lock_ttl.to_string(), round_ttl.to_string()],
            )
            .await;
        match result {
            Ok(1) => MaintenanceLease::Acquired(token),
            Ok(_) => MaintenanceLease::Skipped,
            Err(error) => {
                tracing::warn!(job, %error, "maintenance lease Redis unavailable; failing open");
                MaintenanceLease::RedisUnavailable
            }
        }
    }

    /// 维护任务活跃锁续租（CAS：仅当持有者 token 匹配时刷新 TTL）。
    pub async fn maintenance_extend(&self, job: &str, token: &str, lock_ttl_secs: i64) -> bool {
        static SCRIPT: CachedScript = CachedScript::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('EXPIRE', KEYS[1], ARGV[2]) else return 0 end",
        );
        let renewed: Result<i64, _> = SCRIPT
            .eval(
                &self.client,
                vec![maint_lock_key(job)],
                vec![token.to_owned(), lock_ttl_secs.max(1).to_string()],
            )
            .await;
        matches!(renewed, Ok(1))
    }

    /// 释放维护任务活跃锁（CAS：仅删自己的锁，保留 `maint:round:{job}` 周期占位）。
    pub async fn maintenance_release(&self, job: &str, token: &str) {
        static SCRIPT: CachedScript = CachedScript::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end",
        );
        let _: Result<i64, _> = SCRIPT
            .eval(
                &self.client,
                vec![maint_lock_key(job)],
                vec![token.to_owned()],
            )
            .await;
    }

    /// 跨副本渠道 key 失败脏标记：任一副本登记瞬态失败后写入，其它副本成功时可感知并清零 PG 计数。
    pub async fn health_mark_key_dirty(&self, channel_key_id: i64) {
        self.mark_dirty_member(DIRTY_KEY_SET, channel_key_id).await;
    }

    pub async fn health_clear_key_dirty(&self, channel_key_id: i64) {
        let _: Result<i64, _> = self.client.srem(DIRTY_KEY_SET, channel_key_id).await;
    }

    pub async fn health_dirty_keys(&self) -> Option<HashSet<i64>> {
        self.load_dirty_members(DIRTY_KEY_SET).await
    }

    /// 跨副本出口代理失败脏标记：同上，用于多副本间共享「需清零代理失败计数」状态。
    pub async fn health_mark_egress_dirty(&self, proxy_id: i64) {
        self.mark_dirty_member(DIRTY_EGRESS_SET, proxy_id).await;
    }

    pub async fn health_clear_egress_dirty(&self, proxy_id: i64) {
        let _: Result<i64, _> = self.client.srem(DIRTY_EGRESS_SET, proxy_id).await;
    }

    pub async fn health_dirty_egress(&self) -> Option<HashSet<i64>> {
        self.load_dirty_members(DIRTY_EGRESS_SET).await
    }

    async fn mark_dirty_member(&self, key: &'static str, id: i64) {
        let pipe = self.client.pipeline();
        let _: Result<(), _> = pipe.sadd(key, id).await;
        let _: Result<(), _> = pipe.expire(key, DIRTY_SET_TTL_SECS, None).await;
        let _: Result<Vec<fred::types::Value>, _> = pipe.all().await;
    }

    async fn load_dirty_members(&self, key: &'static str) -> Option<HashSet<i64>> {
        let members: Vec<String> = self.client.smembers(key).await.ok()?;
        Some(
            members
                .into_iter()
                .filter_map(|raw| raw.parse::<i64>().ok())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_keys_share_redis_cluster_hash_tag() {
        fn tag(key: &str) -> &str {
            let start = key.find('{').expect("opening brace");
            let end = key[start + 1..].find('}').expect("closing brace") + start + 1;
            &key[start + 1..end]
        }
        let lock = maint_lock_key("reconcile");
        let round = maint_round_key("reconcile");
        assert_eq!(lock, "lock:maint:{reconcile}");
        assert_eq!(round, "maint:round:{reconcile}");
        assert_eq!(tag(&lock), tag(&round));
    }
}
