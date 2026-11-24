//! 鉴权缓存（docs/database.md §2.1 `auth:key:<sha256>`，60s TTL，带全局版本 + 单 key 版本）。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;
use okapi_store::AuthedKey;
use serde::{Deserialize, Serialize};

const AUTH_CACHE_SCHEMA: i16 = 4;
const AUTH_CACHE_TTL_SECS: i64 = 60;
/// 单 key 版本键的滑动 TTL：必须严格大于 `AUTH_CACHE_TTL_SECS`，确保版本键过期前任何旧缓存早已自然失效。
const AUTH_KEY_VER_TTL_SECS: i64 = 120;

fn auth_cache_key(key_hash: &str) -> String {
    format!("auth:key:{key_hash}")
}

fn auth_key_ver_key(key_hash: &str) -> String {
    format!("auth:kv:{key_hash}")
}

fn parse_ver(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0)
}

/// 鉴权缓存双版本号（全局 `auth:ver` + 单 key `auth:kv:<hash>`），用于防回源竞态且避免单 key 失效冲掉全站缓存。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuthVersion {
    pub global: i64,
    pub key: i64,
}

impl SchedulerRedis {
    // ---- 鉴权缓存（docs/database.md §2.1 auth:key:*，60s TTL）----
    // 双层失效模型：
    // - 全局版本键 `auth:ver`：角色/分组/用户封禁等全局变更走 `auth_flush`（INCR 即 O(1) 全量失效）；
    // - 单 key 版本键 `auth:kv:<hash>`：单 key 禁用/轮换/删除/会话退出走 `auth_del`（仅递增该 key 滑动版本并删缓存，
    //   既防并发回源把旧快照写回缓存，又不再连坐冲掉全集群其他用户的鉴权缓存）。

    /// 读鉴权缓存（全局版本、单 key 版本或 schema 任一不匹配均视为 miss）。
    pub async fn auth_get(&self, key_hash: &str) -> Option<AuthedKey> {
        let keys = vec![
            auth_cache_key(key_hash),
            "auth:ver".to_owned(),
            auth_key_ver_key(key_hash),
        ];
        let values: Vec<Option<String>> = self.get_many(keys).await.ok()?;
        let payload = values.first()?.as_deref()?;
        let current = AuthVersion {
            global: parse_ver(values.get(1).and_then(Option::as_deref)),
            key: parse_ver(values.get(2).and_then(Option::as_deref)),
        };
        decode_if_valid(payload, current)
    }

    /// Capture before reading PG; never stamp an old snapshot with a newer version.
    pub async fn auth_version(&self, key_hash: &str) -> Option<AuthVersion> {
        let keys = vec!["auth:ver".to_owned(), auth_key_ver_key(key_hash)];
        let values: Vec<Option<String>> = self.get_many(keys).await.ok()?;
        Some(AuthVersion {
            global: parse_ver(values.first().and_then(Option::as_deref)),
            key: parse_ver(values.get(1).and_then(Option::as_deref)),
        })
    }

    pub async fn auth_set(&self, key_hash: &str, key: &AuthedKey, version: AuthVersion) {
        let entry = AuthCacheEntry {
            ver: version.global,
            key_ver: version.key,
            schema: AUTH_CACHE_SCHEMA,
            key: key.clone(),
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            let _: Result<(), _> = self
                .client
                .set(
                    auth_cache_key(key_hash),
                    json,
                    Some(Expiration::EX(AUTH_CACHE_TTL_SECS)),
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

    /// 单 key 精确失效（key 禁用/轮换/删除/登出场景）：
    /// 仅递增该 key 的滑动版本号并删除其缓存键，不再触发全站 `auth_flush`。
    pub async fn auth_del(&self, key_hash: &str) {
        let kv_key = auth_key_ver_key(key_hash);
        let cache_key = auth_cache_key(key_hash);
        let (bumped, _) = tokio::join!(
            self.incr_sliding(&kv_key, AUTH_KEY_VER_TTL_SECS),
            self.client.del::<i64, _>(&cache_key),
        );
        if let Err(err) = bumped {
            tracing::warn!(error = %err, "auth_del 单 key 版本递增失败（依赖 60s TTL 兜底）");
        }
    }
}

fn decode_if_valid(payload: &str, current: AuthVersion) -> Option<AuthedKey> {
    let entry: AuthCacheEntry = serde_json::from_str(payload).ok()?;
    // schema 4：条目带双版本号（全局 ver + 单 key key_ver）；旧条目（schema 3）一律当未命中回源
    if entry.ver != current.global
        || entry.key_ver != current.key
        || entry.schema != AUTH_CACHE_SCHEMA
    {
        return None;
    }
    Some(entry.key)
}

#[derive(Serialize, Deserialize)]
struct AuthCacheEntry {
    ver: i64,
    #[serde(default)]
    key_ver: i64,
    schema: i16,
    key: AuthedKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> AuthedKey {
        AuthedKey {
            key_id: 7,
            user_id: 42,
            key_status: 1,
            quota_limited: false,
            user_status: 1,
            role: 1,
            permissions: None,
            pool_code: Some("default".into()),
            pool_strategy: Some("priority_weighted".into()),
            pool_fallback: None,
            group_code: "default".into(),
            group_rpm_limit: None,
            group_rph_limit: None,
            multiplier_scaled: 1_000_000,
            rpm_limit: None,
            tpm_limit: None,
            rpd_limit: None,
            daily_token_limit: None,
            daily_quota_micro: None,
            monthly_quota_micro: None,
            max_concurrency: None,
            model_allowlist: None,
            ip_allowlist: None,
            expires_at: None,
            member_user_id: None,
            member_monthly_limit_micro: None,
            session_hash: None,
        }
    }

    #[test]
    fn decode_requires_matching_global_and_per_key_versions_and_schema() {
        let entry = AuthCacheEntry {
            ver: 3,
            key_ver: 2,
            schema: AUTH_CACHE_SCHEMA,
            key: sample_key(),
        };
        let payload = serde_json::to_string(&entry).expect("serialize");

        assert!(decode_if_valid(&payload, AuthVersion { global: 3, key: 2 }).is_some());
        assert!(
            decode_if_valid(&payload, AuthVersion { global: 4, key: 2 }).is_none(),
            "global auth_flush must invalidate entry"
        );
        assert!(
            decode_if_valid(&payload, AuthVersion { global: 3, key: 3 }).is_none(),
            "per-key auth_del must invalidate entry even when global version is unchanged"
        );

        let legacy = AuthCacheEntry {
            ver: 3,
            key_ver: 0,
            schema: 3,
            key: sample_key(),
        };
        let legacy_payload = serde_json::to_string(&legacy).expect("serialize");
        assert!(
            decode_if_valid(&legacy_payload, AuthVersion { global: 3, key: 0 }).is_none(),
            "schema 3 entry must miss during rolling upgrade"
        );
    }
}
