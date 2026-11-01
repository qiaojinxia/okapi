//! 用户侧限流与累计：用户×模型 RPM、分组 RPM/RPH、团成员月消费、用户月 tokens / 月消费。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;

impl SchedulerRedis {
    /// 用户×模型 RPM（§11.1 new-api 吸收）。
    pub async fn model_rate_ok(&self, user_id: i64, model: &str, limit: i64) -> bool {
        let minute = chrono::Utc::now().timestamp() / 60;
        let key = format!("rl:{{{user_id}}}:m:{model}:rpm:{minute}");
        self.fixed_window_ok(&key, 120, limit).await
    }

    /// 分组级限流（§11.32）：分组内每用户的分钟 / 小时固定窗。
    /// 返回超限的那一轴（`group_rpm` / `group_rph`）供错误 param；None = 放行。
    /// 未配置的轴不产生 Redis 往返。
    pub async fn group_rate_check(
        &self,
        user_id: i64,
        group: &str,
        rpm: Option<i64>,
        rph: Option<i64>,
    ) -> Option<&'static str> {
        let now = chrono::Utc::now().timestamp();
        if let Some(limit) = rpm {
            let key = format!("rl:{{{user_id}}}:g:{group}:rpm:{}", now / 60);
            if !self.fixed_window_ok(&key, 120, limit).await {
                return Some("group_rpm");
            }
        }
        if let Some(limit) = rph {
            let key = format!("rl:{{{user_id}}}:g:{group}:rph:{}", now / 3600);
            if !self.fixed_window_ok(&key, 7200, limit).await {
                return Some("group_rph");
            }
        }
        None
    }

    /// 团成员本月消费计数（软实时限额语义，IMPLEMENTATION §6.1）。
    pub async fn member_spend_get(&self, team: i64, member: i64) -> i64 {
        let key = Self::member_spend_key(team, member);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 结算后累加（40d TTL 覆盖整月 + 复核余量）。
    pub async fn member_spend_add(&self, team: i64, member: i64, amount_micro: i64) {
        if amount_micro <= 0 {
            return;
        }
        let key = Self::member_spend_key(team, member);
        if let Err(err) = self
            .incr_by_with_ttl(&key, amount_micro, 40 * 24 * 3600)
            .await
        {
            tracing::warn!(team, member, error = %err, "团队成员月消费累加失败，成员月限额少计");
        }
    }

    fn member_spend_key(team: i64, member: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("spend:tm:{team}:{member}:{month}")
    }

    /// 用户本月累计 token（volume 规则唯一输入，docs/database.md §2.1）。
    /// 读失败按 0 返回——量级折扣宁可不打，也不能因 Redis 抖动错算。
    pub async fn monthly_tokens_get(&self, user_id: i64) -> u64 {
        let key = Self::monthly_tokens_key(user_id);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 结算后累加实际 usage 总量（40d TTL 覆盖整月 + 复核余量）。
    pub async fn monthly_tokens_add(&self, user_id: i64, tokens: u64) {
        let Ok(delta) = i64::try_from(tokens) else {
            return;
        };
        if delta <= 0 {
            return;
        }
        let key = Self::monthly_tokens_key(user_id);
        if let Err(err) = self.incr_by_with_ttl(&key, delta, 40 * 24 * 3600).await {
            tracing::warn!(user_id, error = %err, "月累计 token 累加失败，量级规则少计");
        }
    }

    fn monthly_tokens_key(user_id: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("tok:{{{user_id}}}:{month}")
    }

    /// 用户本月累计消费 micro（volume 规则消费额轴输入；语义与 tok 计数同构：
    /// 结算后累加、报价前读取，读失败按 0 = 不打折，宁少算不错算）。
    pub async fn monthly_spend_get(&self, user_id: i64) -> u64 {
        let key = Self::monthly_spend_key(user_id);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 结算后累加实付 micro（40d TTL 覆盖整月 + 复核余量）。
    pub async fn monthly_spend_add(&self, user_id: i64, amount_micro: i64) {
        if amount_micro <= 0 {
            return;
        }
        let key = Self::monthly_spend_key(user_id);
        if let Err(err) = self
            .incr_by_with_ttl(&key, amount_micro, 40 * 24 * 3600)
            .await
        {
            tracing::warn!(user_id, error = %err, "月累计消费累加失败，量级规则少计");
        }
    }

    fn monthly_spend_key(user_id: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("usd:{{{user_id}}}:{month}")
    }
}
