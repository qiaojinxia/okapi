//! 用户侧限流与累计：用户×模型 RPM、分组 RPM/RPH、团成员月消费、用户月 tokens / 月消费。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use okapi_store::CachedScript;

impl SchedulerRedis {
    fn model_rpm_key(user_id: i64, model: &str, now: i64) -> String {
        format!("rl:{{{user_id}}}:m:{model}:rpm:{}", now / 60)
    }

    fn group_rpm_key(user_id: i64, group: &str, now: i64) -> String {
        format!("rl:{{{user_id}}}:g:{group}:rpm:{}", now / 60)
    }

    fn group_rph_key(user_id: i64, group: &str, now: i64) -> String {
        format!("rl:{{{user_id}}}:g:{group}:rph:{}", now / 3600)
    }

    /// 多轴固定窗原子限流：所有 key 必须共享 `{user_id}` Cluster hash-tag。
    /// 先逐轴检查 `cur >= limit`，任一轴超限直接返回该轴名且**不递增任何计数**（消除被拒请求的配额空耗）；
    /// 全部通过后在同一条 Lua 脚本内原子递增并挂 TTL，单次 Redis 往返完成。
    /// Redis 故障保持 fail-open（与 `fixed_window_ok` 一致）。
    async fn atomic_user_windows_check(
        &self,
        windows: &[(String, i64, i64, &'static str)],
    ) -> Option<&'static str> {
        static SCRIPT: CachedScript = CachedScript::new(
            r"
            for i = 1, #KEYS do
                local limit = tonumber(ARGV[(i - 1) * 2 + 1])
                local cur = tonumber(redis.call('GET', KEYS[i]) or '0')
                if cur >= limit then
                    return i
                end
            end
            for i = 1, #KEYS do
                local ttl = tonumber(ARGV[(i - 1) * 2 + 2])
                local n = redis.call('INCR', KEYS[i])
                if n == 1 or redis.call('TTL', KEYS[i]) == -1 then
                    redis.call('EXPIRE', KEYS[i], ttl)
                end
            end
            return 0
            ",
        );
        if windows.is_empty() {
            return None;
        }
        let mut keys = Vec::with_capacity(windows.len());
        let mut args = Vec::with_capacity(windows.len() * 2);
        for (key, ttl_secs, limit, _) in windows {
            keys.push(key.clone());
            args.push(limit.to_string());
            args.push(ttl_secs.to_string());
        }
        match SCRIPT.eval::<i64, _, _>(&self.client, keys, args).await {
            Ok(code) => decode_window_rejection(code, windows),
            Err(err) => {
                tracing::debug!(error = %err, "多轴固定窗原子限流失败（放行）");
                None
            }
        }
    }

    /// 用户×模型 RPM（§11.1 new-api 吸收）。
    pub async fn model_rate_ok(&self, user_id: i64, model: &str, limit: i64) -> bool {
        let now = chrono::Utc::now().timestamp();
        let key = Self::model_rpm_key(user_id, model, now);
        self.fixed_window_ok(&key, 120, limit).await
    }

    /// 分组级限流（§11.32）：分组内每用户的分钟 / 小时固定窗，单次原子 Lua 检查与递增。
    /// 返回超限的那一轴（`group_rpm` / `group_rph`）供错误 param；None = 放行。
    /// 未配置的轴不产生 Redis 往返。
    pub async fn group_rate_check(
        &self,
        user_id: i64,
        group: &str,
        rpm: Option<i64>,
        rph: Option<i64>,
    ) -> Option<&'static str> {
        self.group_and_model_rate_check(user_id, group, rpm, rph, None)
            .await
    }

    /// 合并分组级 `[group_rpm, group_rph]` 与模型级 `model_rpm` 为单次原子 Redis Lua 往返：
    /// 三把键同属 `{user_id}` Cluster hash-tag，严格按 `group_rpm -> group_rph -> model_rpm`
    /// 顺序校验；任一超限均不扣减其他轴配额；全部未配置时零往返。
    pub async fn group_and_model_rate_check(
        &self,
        user_id: i64,
        group: &str,
        rpm: Option<i64>,
        rph: Option<i64>,
        model_rpm: Option<(&str, i64)>,
    ) -> Option<&'static str> {
        let now = chrono::Utc::now().timestamp();
        let mut windows = Vec::with_capacity(3);
        if let Some(limit) = rpm {
            windows.push((Self::group_rpm_key(user_id, group, now), 120, limit, "group_rpm"));
        }
        if let Some(limit) = rph {
            windows.push((Self::group_rph_key(user_id, group, now), 7200, limit, "group_rph"));
        }
        if let Some((model, limit)) = model_rpm {
            windows.push((Self::model_rpm_key(user_id, model, now), 120, limit, "model_rpm"));
        }
        self.atomic_user_windows_check(&windows).await
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

    /// 令牌当日累计消费 micro（UTC 自然日，软实时周期预算语义）。
    pub async fn key_spend_day_get(&self, user_id: i64, key_id: i64) -> i64 {
        let key = Self::key_spend_day_key(user_id, key_id);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 令牌当月累计消费 micro（UTC 自然月，软实时周期预算语义）。
    pub async fn key_spend_month_get(&self, user_id: i64, key_id: i64) -> i64 {
        let key = Self::key_spend_month_key(user_id, key_id);
        let value: Option<String> = self.client.get(&key).await.ok().flatten();
        value.and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// 单次管道读取请求准入所需的消费计数（团成员月消费 / 令牌日消费 / 令牌月消费）。
    /// 仅查询已配置上限的键；全部未配置时零 Redis 往返。
    pub async fn key_limits_spend_get(
        &self,
        user_id: i64,
        key_id: i64,
        member: Option<i64>,
        need_day: bool,
        need_month: bool,
    ) -> (i64, i64, i64) {
        let mut keys = Vec::with_capacity(3);
        let member_idx = member.map(|m| {
            let idx = keys.len();
            keys.push(Self::member_spend_key(user_id, m));
            idx
        });
        let day_idx = need_day.then(|| {
            let idx = keys.len();
            keys.push(Self::key_spend_day_key(user_id, key_id));
            idx
        });
        let month_idx = need_month.then(|| {
            let idx = keys.len();
            keys.push(Self::key_spend_month_key(user_id, key_id));
            idx
        });
        if keys.is_empty() {
            return (0, 0, 0);
        }
        let values: Vec<Option<String>> = self.get_many(keys).await.unwrap_or_default();
        let parse_at = |idx: Option<usize>| -> i64 {
            idx.and_then(|i| values.get(i))
                .and_then(Option::as_deref)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0)
        };
        (parse_at(member_idx), parse_at(day_idx), parse_at(month_idx))
    }

    /// 结算后原子累加令牌日/月消费（同 `{user_id}` Cluster hash-tag 单次 Lua 往返；
    /// 日桶 48h TTL、月桶 40d TTL；写失败宁少计不误挡）。
    pub async fn key_spend_add(&self, user_id: i64, key_id: i64, amount_micro: i64) {
        static SCRIPT: CachedScript = CachedScript::new(
            r"
            local delta = tonumber(ARGV[1])
            local d = redis.call('INCRBY', KEYS[1], delta)
            if d == delta or redis.call('TTL', KEYS[1]) == -1 then
                redis.call('EXPIRE', KEYS[1], tonumber(ARGV[2]))
            end
            local m = redis.call('INCRBY', KEYS[2], delta)
            if m == delta or redis.call('TTL', KEYS[2]) == -1 then
                redis.call('EXPIRE', KEYS[2], tonumber(ARGV[3]))
            end
            return 1
            ",
        );
        if key_id <= 0 || amount_micro <= 0 {
            return;
        }
        let day_key = Self::key_spend_day_key(user_id, key_id);
        let month_key = Self::key_spend_month_key(user_id, key_id);
        if let Err(err) = SCRIPT
            .eval::<i64, _, _>(
                &self.client,
                vec![day_key, month_key],
                vec![
                    amount_micro.to_string(),
                    (48 * 3600).to_string(),
                    (40 * 24 * 3600).to_string(),
                ],
            )
            .await
        {
            tracing::warn!(user_id, key_id, error = %err, "令牌日/月消费累加失败");
        }
    }

    fn key_spend_day_key(user_id: i64, key_id: i64) -> String {
        let day = chrono::Utc::now().format("%Y%m%d");
        format!("spend:{{{user_id}}}:k:{key_id}:d:{day}")
    }

    fn key_spend_month_key(user_id: i64, key_id: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("spend:{{{user_id}}}:k:{key_id}:m:{month}")
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

    /// 单次管道读取本月累计 token 与本月累计消费 micro（volume 规则双轴合并读，未启用的轴跳过）。
    pub async fn monthly_volume_get(
        &self,
        user_id: i64,
        need_tokens: bool,
        need_spend: bool,
    ) -> (u64, u64) {
        let mut keys = Vec::with_capacity(2);
        let tok_idx = need_tokens.then(|| {
            let idx = keys.len();
            keys.push(Self::monthly_tokens_key(user_id));
            idx
        });
        let spend_idx = need_spend.then(|| {
            let idx = keys.len();
            keys.push(Self::monthly_spend_key(user_id));
            idx
        });
        if keys.is_empty() {
            return (0, 0);
        }
        let values: Vec<Option<String>> = self.get_many(keys).await.unwrap_or_default();
        let parse_at = |idx: Option<usize>| -> u64 {
            idx.and_then(|i| values.get(i))
                .and_then(Option::as_deref)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };
        (parse_at(tok_idx), parse_at(spend_idx))
    }

    fn monthly_spend_key(user_id: i64) -> String {
        let month = chrono::Utc::now().format("%Y%m");
        format!("usd:{{{user_id}}}:{month}")
    }
}

fn decode_window_rejection(
    code: i64,
    windows: &[(String, i64, i64, &'static str)],
) -> Option<&'static str> {
    if code <= 0 {
        return None;
    }
    usize::try_from(code - 1)
        .ok()
        .and_then(|idx| windows.get(idx))
        .map(|w| w.3)
}

#[cfg(test)]
mod tests {
    use super::{SchedulerRedis, decode_window_rejection};

    fn cluster_tag(key: &str) -> &str {
        let start = key.find('{').expect("opening brace");
        let end = key[start + 1..].find('}').expect("closing brace") + start + 1;
        &key[start + 1..end]
    }

    #[test]
    fn key_spend_day_and_month_keys_share_cluster_hash_tag() {
        let day = SchedulerRedis::key_spend_day_key(42, 9);
        let month = SchedulerRedis::key_spend_month_key(42, 9);
        assert_eq!(cluster_tag(&day), "42");
        assert_eq!(cluster_tag(&day), cluster_tag(&month));
    }

    #[test]
    fn group_and_model_rate_keys_share_user_cluster_hash_tag() {
        let now = 1_760_000_000;
        let keys = [
            SchedulerRedis::group_rpm_key(77, "pro", now),
            SchedulerRedis::group_rph_key(77, "pro", now),
            SchedulerRedis::model_rpm_key(77, "gpt-5", now),
            SchedulerRedis::monthly_tokens_key(77),
            SchedulerRedis::monthly_spend_key(77),
        ];
        for key in &keys {
            assert_eq!(cluster_tag(key), "77", "key={key}");
        }
    }

    #[test]
    fn decode_window_rejection_maps_lua_indices_accurately() {
        let windows = vec![
            ("k1".to_owned(), 120, 10, "group_rpm"),
            ("k2".to_owned(), 7200, 100, "group_rph"),
            ("k3".to_owned(), 120, 5, "model_rpm"),
        ];
        assert_eq!(decode_window_rejection(0, &windows), None);
        assert_eq!(decode_window_rejection(-1, &windows), None);
        assert_eq!(decode_window_rejection(1, &windows), Some("group_rpm"));
        assert_eq!(decode_window_rejection(2, &windows), Some("group_rph"));
        assert_eq!(decode_window_rejection(3, &windows), Some("model_rpm"));
        assert_eq!(decode_window_rejection(4, &windows), None);
    }
}
