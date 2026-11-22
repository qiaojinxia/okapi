//! 渠道 key 的选路输入：key 级 RPM 窗、当日消费、在途、时延 EWMA，以及负毛利熔断名单。
use super::SchedulerRedis;
use fred::interfaces::{HashesInterface, KeysInterface, SortedSetsInterface};

/// 渠道 key 近 10 分钟（双 5 分钟桶）滚动质量读数：成功数、失败数与实时时延 EWMA。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyQualitySample {
    pub ok: u32,
    pub err: u32,
    pub latency_ms: Option<u32>,
}

impl KeyQualitySample {
    #[must_use]
    pub const fn total(self) -> u32 {
        self.ok.saturating_add(self.err)
    }

    /// 滚动错误率（千分比，0..=1000）；无样本时为 0。
    #[must_use]
    pub fn error_rate_milli(self) -> u16 {
        let total = u64::from(self.total());
        if total == 0 {
            return 0;
        }
        let rate = (u64::from(self.err) * 1_000 / total).min(1_000);
        u16::try_from(rate).unwrap_or(1_000)
    }
}

/// key 的并发租约集合：成员是一次在途尝试，分数是租约到期的毫秒时刻。
#[must_use]
pub fn channel_lease_key(key: i64) -> String {
    format!("conc:ck:{{{key}}}:v2")
}

impl SchedulerRedis {
    /// 渠道 key 级 RPM 闸（`channel_keys.rpm_limit`）。
    ///
    /// 超限返回 false，调用方把该 key 摘出候选而不是拒绝整个请求——同渠道其它 key
    /// 仍可承接。
    pub async fn channel_key_rate_ok(&self, channel_key_id: i64, limit: i64) -> bool {
        let minute = chrono::Utc::now().timestamp() / 60;
        let key = format!("rpm:ck:{channel_key_id}:{minute}");
        self.fixed_window_ok(&key, 120, limit).await
    }

    /// 负毛利熔断表整读（§11.34，`mb:blocks`）；Redis 故障 None = 调用方按不熔处理。
    pub async fn margin_blocks(
        &self,
    ) -> Option<std::collections::HashMap<String, crate::margin::BlockEntry>> {
        crate::margin::load_blocks(&self.client).await
    }

    /// 写一条熔断表条目（管理面解除 / 测试用）。
    pub async fn margin_block_set(
        &self,
        field: &str,
        entry: &crate::margin::BlockEntry,
    ) -> Result<(), fred::error::Error> {
        crate::margin::set_block(&self.client, field, entry).await
    }

    /// 渠道 key 当日累计消费（micro）。读不到按 0 处理 = 不拦。
    pub async fn channel_key_spend_get(&self, channel_key_id: i64) -> i64 {
        let key = Self::channel_key_spend_key(channel_key_id);
        self.client
            .get::<Option<i64>, _>(&key)
            .await
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// 结算后累加渠道 key 当日消费。软实时：先花后记，可能略超上限。
    pub async fn channel_key_spend_add(&self, channel_key_id: i64, amount_micro: i64) {
        let key = Self::channel_key_spend_key(channel_key_id);
        // warn 而非 debug：累加失败时渠道 key 日消费上限会静默失效（debug 不进运维日志）
        if let Err(err) = self.incr_by_with_ttl(&key, amount_micro, 172_800).await {
            tracing::warn!(channel_key_id, error = %err, "channel_key_spend 累加失败，渠道 key 日消费上限少计");
        }
    }

    fn channel_key_spend_key(channel_key_id: i64) -> String {
        let day = chrono::Utc::now().format("%Y%m%d");
        format!("spend:ck:{channel_key_id}:{day}")
    }

    /// `least_usage` 池的读数：候选 key 当日消费（micro），一次往返。没有记录的 key 不在结果里
    /// （调度按 0 处理）；Redis 读失败时整份为空，退化为按权重随机。
    pub async fn channel_key_spends(
        &self,
        ids: impl Iterator<Item = i64>,
    ) -> std::collections::HashMap<i64, i64> {
        let ids: Vec<_> = ids.collect();
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let keys = ids
            .iter()
            .map(|id| Self::channel_key_spend_key(*id))
            .collect();
        let values: Vec<Option<i64>> = self.get_many(keys).await.unwrap_or_default();
        ids.into_iter()
            .zip(values)
            .filter_map(|(id, value)| value.map(|micro| (id, micro)))
            .collect()
    }

    /// `least_concurrency` 池的读数：候选 key 未到期的在途租约数（`conc:ck:*`），一次往返。
    /// 读失败时整份为空，退化为按权重随机。
    pub async fn channel_key_in_flight(
        &self,
        ids: impl Iterator<Item = i64>,
    ) -> std::collections::HashMap<i64, i64> {
        let ids: Vec<_> = ids.collect();
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }
        #[allow(clippy::cast_precision_loss)] // 毫秒时间戳远小于 2^53
        let now = chrono::Utc::now().timestamp_millis() as f64;
        let pipe = self.client.pipeline();
        for id in &ids {
            if pipe
                .zcount::<(), _>(channel_lease_key(*id), now, f64::MAX)
                .await
                .is_err()
            {
                return std::collections::HashMap::new();
            }
        }
        let counts: Vec<i64> = pipe.all().await.unwrap_or_default();
        ids.into_iter()
            .zip(counts)
            .filter(|(_, count)| *count > 0)
            .collect()
    }

    /// 渠道 key 时延 EWMA（毫秒）。无样本返回 None，调用方按中位数处理，
    /// 避免新 key 因"没有历史"被永久冷落，也避免被误判为最快而被灌流。
    pub async fn channel_key_latency(&self, channel_key_id: i64) -> Option<u32> {
        self.client
            .get::<Option<u32>, _>(&format!("lat:ck:{channel_key_id}"))
            .await
            .ok()
            .flatten()
    }

    /// One Redis round trip for the complete candidate set.
    pub async fn channel_key_latencies(
        &self,
        ids: impl Iterator<Item = i64>,
    ) -> std::collections::HashMap<i64, u32> {
        let ids: Vec<_> = ids.collect();
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let keys: Vec<_> = ids.iter().map(|id| format!("lat:ck:{id}")).collect();
        let values: Vec<Option<u32>> = self.get_many(keys).await.unwrap_or_default();
        ids.into_iter()
            .zip(values)
            .filter_map(|(id, value)| value.map(|ms| (id, ms)))
            .collect()
    }

    /// 更新时延 EWMA：`new = old * 0.7 + sample * 0.3`（整数运算，非计费路径）。
    /// 权重偏向历史，单次抖动不足以改变选路；10min TTL 让长期不用的 key 自然回到无样本。
    pub async fn channel_key_latency_record(&self, channel_key_id: i64, sample_ms: u32) {
        let key = format!("lat:ck:{channel_key_id}");
        let next = match self.client.get::<Option<u32>, _>(&key).await {
            Ok(Some(old)) => (u64::from(old) * 7 + u64::from(sample_ms) * 3) / 10,
            _ => u64::from(sample_ms),
        };
        let next = u32::try_from(next).unwrap_or(u32::MAX);
        if let Err(err) = self
            .client
            .set::<(), _, _>(
                &key,
                next,
                Some(fred::types::Expiration::EX(600)),
                None,
                false,
            )
            .await
        {
            tracing::debug!(error = %err, "时延 EWMA 写入失败");
        }
    }

    fn channel_key_quality_bucket(channel_key_id: i64, bucket: i64) -> String {
        format!("qual:ck:{channel_key_id}:{bucket}")
    }

    /// 登记一次渠道 key 的成功/失败质量样本（5 分钟桶 + 600s TTL，供低质量自动降级读取）。
    pub async fn channel_key_quality_record(&self, channel_key_id: i64, ok: bool) {
        let bucket = chrono::Utc::now().timestamp() / 300;
        let key = Self::channel_key_quality_bucket(channel_key_id, bucket);
        let field = if ok { "ok" } else { "err" };
        let pipe = self.client.pipeline();
        if pipe.hincrby::<(), _, _>(&key, field, 1).await.is_err()
            || pipe.expire::<(), _>(&key, 600, None).await.is_err()
        {
            return;
        }
        if let Err(err) = pipe.last::<()>().await {
            tracing::debug!(channel_key_id, error = %err, "质量样本写入失败");
        }
    }

    /// 批量读取候选集合的双桶（当前 + 上一个 5 分钟桶）质量统计与时延 EWMA；
    /// 单次管道往返，Redis 故障时返回空表（安全退化为不降级）。
    pub async fn channel_key_qualities(
        &self,
        ids: impl Iterator<Item = i64>,
    ) -> std::collections::HashMap<i64, KeyQualitySample> {
        let ids: Vec<i64> = ids.collect();
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let now_bucket = chrono::Utc::now().timestamp() / 300;
        let prev_bucket = now_bucket - 1;

        let mut qual_keys = Vec::with_capacity(ids.len() * 2);
        for &id in &ids {
            qual_keys.push(Self::channel_key_quality_bucket(id, now_bucket));
            qual_keys.push(Self::channel_key_quality_bucket(id, prev_bucket));
        }

        let pipe = self.client.pipeline();
        for key in &qual_keys {
            if pipe
                .hmget::<(), _, _>(key, vec!["ok", "err"])
                .await
                .is_err()
            {
                return std::collections::HashMap::new();
            }
        }
        let counts: Vec<Vec<Option<u32>>> = pipe.all().await.unwrap_or_default();
        let latencies = self.channel_key_latencies(ids.iter().copied()).await;

        let mut out = std::collections::HashMap::with_capacity(ids.len());
        for (idx, id) in ids.into_iter().enumerate() {
            let mut ok = 0u32;
            let mut err = 0u32;
            for offset in [0usize, 1usize] {
                if let Some(pair) = counts.get(idx * 2 + offset) {
                    ok = ok.saturating_add(pair.first().copied().flatten().unwrap_or(0));
                    err = err.saturating_add(pair.get(1).copied().flatten().unwrap_or(0));
                }
            }
            let latency_ms = latencies.get(&id).copied();
            if ok > 0 || err > 0 || latency_ms.is_some() {
                out.insert(
                    id,
                    KeyQualitySample {
                        ok,
                        err,
                        latency_ms,
                    },
                );
            }
        }
        out
    }
}
