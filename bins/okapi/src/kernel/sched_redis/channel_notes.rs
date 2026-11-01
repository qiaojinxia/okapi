//! 渠道测活 / 余额查询的最近一次结果（列表页展示用）。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;

impl SchedulerRedis {
    /// 记录渠道最近一次测活结果（`ch:test:<channel_id>`，30 天 TTL）。
    /// 提示性信息不进 PG：new-api 把 response_time/test_time 存在 channels 表上，
    /// 我们用 Redis——它天然会过期，列表上不会挂着半年前的"200ms"误导人。
    pub async fn channel_test_record(&self, channel_id: i64, result: &serde_json::Value) {
        self.channel_note_record("ch:test", channel_id, result)
            .await;
    }

    /// 记录渠道最近一次上游余额查询结果（`ch:balance:<channel_id>`，30 天 TTL，§11.33）。
    pub async fn channel_balance_record(&self, channel_id: i64, result: &serde_json::Value) {
        self.channel_note_record("ch:balance", channel_id, result)
            .await;
    }

    async fn channel_note_record(&self, prefix: &str, channel_id: i64, result: &serde_json::Value) {
        let key = format!("{prefix}:{channel_id}");
        let value = result.to_string();
        if let Err(err) = self
            .client
            .set::<(), _, _>(
                &key,
                value,
                Some(Expiration::EX(30 * 24 * 3600)),
                None,
                false,
            )
            .await
        {
            tracing::debug!(error = %err, key, "渠道留痕写入失败（忽略）");
        }
    }

    /// 批量读最近测活结果（列表页一次 MGET 回填所有行；读失败按空处理）。
    pub async fn channel_test_get_many(
        &self,
        channel_ids: &[i64],
    ) -> std::collections::HashMap<i64, serde_json::Value> {
        self.channel_note_get_many("ch:test", channel_ids).await
    }

    /// 批量读最近余额查询结果（同 MGET 回填）。
    pub async fn channel_balance_get_many(
        &self,
        channel_ids: &[i64],
    ) -> std::collections::HashMap<i64, serde_json::Value> {
        self.channel_note_get_many("ch:balance", channel_ids).await
    }

    async fn channel_note_get_many(
        &self,
        prefix: &str,
        channel_ids: &[i64],
    ) -> std::collections::HashMap<i64, serde_json::Value> {
        if channel_ids.is_empty() {
            return std::collections::HashMap::new();
        }
        let keys: Vec<String> = channel_ids
            .iter()
            .map(|id| format!("{prefix}:{id}"))
            .collect();
        let values: Vec<Option<String>> = self.get_many(keys).await.unwrap_or_default();
        channel_ids
            .iter()
            .zip(values)
            .filter_map(|(id, v)| {
                v.and_then(|s| serde_json::from_str(&s).ok())
                    .map(|parsed| (*id, parsed))
            })
            .collect()
    }
}
