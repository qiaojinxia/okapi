//! 防滥用计数：敏感端点按 IP 限速、发信日配额、兑换码每 IP 次数。
use super::SchedulerRedis;
use fred::interfaces::LuaInterface;

impl SchedulerRedis {
    /// 关键接口每 IP 固定窗计数（60s；对齐 new-api rc.24 关键路由限流）。
    /// 返回窗口内计数；Redis 故障返回 0（放行，与其余限流失败语义一致）。
    pub async fn crit_rate_incr(&self, scope: &str, ip: &str) -> i64 {
        let key = format!("crl:{scope}:{ip}");
        self.incr_with_ttl(&key, 60).await.unwrap_or(0)
    }

    /// 收件箱当日发信计数 +1（24h 窗口，从窗口内第一封起算），返回窗口内累计。
    /// Redis 故障按 0 放行：与每 IP 限流一样，限额是风控增强，不阻断发信主流程。
    pub async fn mail_daily_incr(&self, scope: &str, mailbox: &str) -> i64 {
        let key = format!("mail:day:{scope}:{mailbox}");
        self.incr_with_ttl(&key, 24 * 3600).await.unwrap_or(0)
    }

    /// 兑换码批次 × IP 核销计数 +1（7d 窗口；#1790-5 max_per_ip 闸）。
    pub async fn redeem_ip_incr(&self, batch: uuid::Uuid, ip: &str) -> i64 {
        let key = format!("redeem:ip:{batch}:{ip}");
        // Redis 故障放行（限制是风控增强，不阻核销主流程）
        self.incr_with_ttl(&key, 7 * 24 * 3600).await.unwrap_or(1)
    }

    /// 核销失败回退计数（预查通过但翻转竞争失败时）。
    pub async fn redeem_ip_decr(&self, batch: uuid::Uuid, ip: &str) {
        // 只回退仍在窗口内的计数：对已过期的键 DECR 会凭空建出一个不带过期的 -1
        const LUA: &str = "if redis.call('EXISTS', KEYS[1]) == 1 then return redis.call('DECR', KEYS[1]) end return 0";
        let key = format!("redeem:ip:{batch}:{ip}");
        let _: Result<i64, _> = self.client.eval(LUA, vec![key], Vec::<String>::new()).await;
    }
}
