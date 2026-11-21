//! 跨副本的后台租约：OAuth 凭证刷新锁、出口探测轮次。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use fred::types::{Expiration, SetOptions};
use okapi_store::CachedScript;

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
}
