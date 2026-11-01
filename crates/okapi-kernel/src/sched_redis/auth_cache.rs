//! 鉴权缓存（docs/database.md §2.1 `auth:key:<sha256>`，60s TTL，带全局版本号）。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;
use okapi_store::AuthedKey;
use serde::{Deserialize, Serialize};

impl SchedulerRedis {
    // ---- 鉴权缓存（docs/database.md §2.1 auth:key:<sha256>，60s TTL）----
    // 失效模型：全局版本键 auth:ver，值内嵌写入时版本；INCR 即 O(1) 全量失效，
    // 跨进程立即生效（满足 §2.4 console 精确撤销语义），另有 60s TTL 兜底。

    /// 读鉴权缓存（版本不匹配视为 miss）。
    pub async fn auth_get(&self, key_hash: &str) -> Option<AuthedKey> {
        let keys = vec![format!("auth:key:{key_hash}"), "auth:ver".to_owned()];
        let values: Vec<Option<String>> = self.get_many(keys).await.ok()?;
        let payload = values.first()?.clone()?;
        let current_ver = values
            .get(1)
            .and_then(|v| v.as_deref())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let entry: AuthCacheEntry = serde_json::from_str(&payload).ok()?;
        // schema 3：条目带登录 key 的 session_hash；旧条目一律当未命中回源
        if entry.ver != current_ver || entry.schema != 3 {
            return None;
        }
        Some(entry.key)
    }

    /// Capture before reading PG; never stamp an old snapshot with a newer version.
    pub async fn auth_version(&self) -> Option<i64> {
        self.client
            .get::<Option<i64>, _>("auth:ver")
            .await
            .ok()
            .map(|v| v.unwrap_or(0))
    }

    pub async fn auth_set(&self, key_hash: &str, key: &AuthedKey, ver: i64) {
        let entry = AuthCacheEntry {
            ver,
            schema: 3,
            key: key.clone(),
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            let _: Result<(), _> = self
                .client
                .set(
                    format!("auth:key:{key_hash}"),
                    json,
                    Some(Expiration::EX(60)),
                    None,
                    false,
                )
                .await;
        }
    }

    /// 全量失效（角色/分组等用户级变更后调用；跨进程生效）。
    pub async fn auth_flush(&self) {
        let result: Result<i64, _> = self.client.incr("auth:ver").await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "auth_flush 失败（依赖 60s TTL 兜底）");
        }
    }

    /// 单 key 精确失效（key 禁用/删除场景）。
    pub async fn auth_del(&self, key_hash: &str) {
        self.auth_flush().await;
        let _: Result<i64, _> = self.client.del(format!("auth:key:{key_hash}")).await;
    }
}

#[derive(Serialize, Deserialize)]
struct AuthCacheEntry {
    ver: i64,
    schema: i16,
    key: AuthedKey,
}
