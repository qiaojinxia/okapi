//! 模型级短冷却与 529 过载指数退避（Redis 态，不改动 PG `channel_keys` 表结构）。
//!
//! - `(channel_key, model)` 冷却：用于 `model_not_found` / `DeploymentNotFound` / 地区限制等
//!   单渠道单模型故障，避免误伤同一把 key 上其它可用模型。
//! - `529 overloaded_error` 指数退避：`15s → 30s → 60s → 120s → 300s`（10 分钟无新过载自动衰减），
//!   替代此前硬编码 600s 导致的整池雪崩。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;
use okapi_store::CachedScript;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const OVERLOAD_WINDOW_SECS: i64 = 600;
const OVERLOAD_FALLBACK_SECS: i64 = 30;
static OVERLOAD_STREAK_SCRIPT: CachedScript = CachedScript::new(
    "local n=redis.call('INCR',KEYS[1]); redis.call('EXPIRE',KEYS[1],ARGV[1]); return n",
);

fn model_tag(model: &str) -> String {
    let digest = Sha256::digest(model.as_bytes());
    hex::encode(&digest[..4])
}

/// 连续过载次数 → 退避秒数（15 → 30 → 60 → 120 → 300）。
#[must_use]
pub fn overload_backoff_for_streak(streak: i64) -> i64 {
    match streak {
        ..=1 => 15,
        2 => 30,
        3 => 60,
        4 => 120,
        _ => 300,
    }
}

impl SchedulerRedis {
    fn model_cooldown_key(channel_key_id: i64, model: &str) -> String {
        format!("cool:ck:{{{channel_key_id}}}:m:{}", model_tag(model))
    }

    fn overload_streak_key(channel_key_id: i64) -> String {
        format!("overload:ck:{{{channel_key_id}}}")
    }

    /// 记录 `(channel_key, model)` 级短冷却（不拉黑整把 key，其它模型不受影响）。
    pub async fn model_cooldown_set(&self, channel_key_id: i64, model: &str, secs: i64) {
        if secs <= 0 {
            return;
        }
        let expires_at = chrono::Utc::now().timestamp().saturating_add(secs);
        let key = Self::model_cooldown_key(channel_key_id, model);
        if let Err(err) = self
            .client
            .set::<(), _, _>(
                key,
                expires_at.to_string(),
                Some(Expiration::EX(secs)),
                None,
                false,
            )
            .await
        {
            tracing::debug!(error = %err, channel_key_id, model, "model_cooldown_set 失败（忽略）");
        }
    }

    /// 批量查询候选 key 在该模型上的冷却到期时间戳（单次 pipeline；Redis 不可用时 fail-open 返回空表）。
    pub async fn model_cooldown_active(
        &self,
        model: &str,
        channel_key_ids: &[i64],
    ) -> HashMap<i64, i64> {
        if channel_key_ids.is_empty() {
            return HashMap::new();
        }
        let pipe = self.client.pipeline();
        for &kid in channel_key_ids {
            let key = Self::model_cooldown_key(kid, model);
            if pipe.get::<(), _>(key).await.is_err() {
                return HashMap::new();
            }
        }
        let Ok(values) = pipe.all::<Vec<Option<String>>>().await else {
            return HashMap::new();
        };
        let now = chrono::Utc::now().timestamp();
        let mut active = HashMap::new();
        for (&kid, raw) in channel_key_ids.iter().zip(values.iter()) {
            if let Some(until) = raw
                .as_deref()
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|&until| until > now)
            {
                active.insert(kid, until);
            }
        }
        active
    }

    /// 计算 529 过载的指数退避秒数（连续过载逐步拉长，10 分钟内恢复后清零）。
    pub async fn overload_backoff_secs(&self, channel_key_id: i64) -> i64 {
        let key = Self::overload_streak_key(channel_key_id);
        match OVERLOAD_STREAK_SCRIPT
            .eval::<i64, _, _>(
                &self.client,
                vec![key],
                vec![OVERLOAD_WINDOW_SECS.to_string()],
            )
            .await
        {
            Ok(streak) => overload_backoff_for_streak(streak),
            Err(err) => {
                tracing::debug!(error = %err, channel_key_id, "overload_backoff_secs 失败，回退默认退避");
                OVERLOAD_FALLBACK_SECS
            }
        }
    }

    /// 成功恢复后清除该 key 的过载连续计数。
    pub async fn overload_backoff_reset(&self, channel_key_id: i64) {
        let key = Self::overload_streak_key(channel_key_id);
        let _: Result<i64, _> = self.client.del(key).await;
    }

    /// 运维人工解冷：同时清除指定渠道 key 的 529 退避计数与渠道声明模型的 `(key, model)` 短冷却。
    pub async fn clear_channel_key_cooldowns(&self, channel_key_id: i64, models: &[String]) {
        let mut keys = Vec::with_capacity(models.len() + 1);
        keys.push(Self::overload_streak_key(channel_key_id));
        for model in models {
            keys.push(Self::model_cooldown_key(channel_key_id, model));
        }
        // 同一个 `{channel_key_id}` hash-tag，单次 DEL 在 Redis Cluster 同槽原子执行
        let _: Result<i64, _> = self.client.del(keys).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{SchedulerRedis, overload_backoff_for_streak};

    #[test]
    fn overload_backoff_grows_exponentially_and_caps_at_300s() {
        assert_eq!(overload_backoff_for_streak(0), 15);
        assert_eq!(overload_backoff_for_streak(1), 15);
        assert_eq!(overload_backoff_for_streak(2), 30);
        assert_eq!(overload_backoff_for_streak(3), 60);
        assert_eq!(overload_backoff_for_streak(4), 120);
        assert_eq!(overload_backoff_for_streak(5), 300);
        assert_eq!(overload_backoff_for_streak(20), 300);
    }

    #[test]
    fn model_cooldown_keys_isolate_models_on_same_channel_key() {
        let pro = SchedulerRedis::model_cooldown_key(9, "gemini-2.5-pro");
        let flash = SchedulerRedis::model_cooldown_key(9, "gemini-2.5-flash");
        assert_ne!(pro, flash);
        assert!(pro.starts_with("cool:ck:{9}:m:"));
        assert!(flash.starts_with("cool:ck:{9}:m:"));
    }
}
