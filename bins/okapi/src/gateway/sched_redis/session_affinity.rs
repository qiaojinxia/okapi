//! L2 会话亲和：会话哈希 → 渠道 key 的粘性映射（滑动续期）。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;

/// 会话亲和 TTL（滑动续期）。
const SESSION_TTL_SECS: i64 = 3600;

/// 会话粘性映射的一次命中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StickyHit {
    pub key: i64,
    /// 这个会话上一次成功请求的时刻；旧映射（升级前写的）没有。
    pub last_used: Option<i64>,
}

impl StickyHit {
    fn parse(key: &str, last_used: Option<&str>) -> Option<Self> {
        Some(Self {
            key: key.parse().ok()?,
            last_used: last_used.and_then(|at| at.parse().ok()),
        })
    }

    /// 提示缓存大概率还热：距上次成功不到 `window_secs`。没有时刻（升级前写的映射）按热算，
    /// 保持此前的行为。
    #[must_use]
    pub fn is_warm(&self, now: i64, window_secs: i64) -> bool {
        self.last_used
            .is_none_or(|at| now.saturating_sub(at) <= window_secs)
    }
}

impl SchedulerRedis {
    fn sess_key(user_id: i64, hash: &str) -> String {
        format!("stick:sess:{{{user_id}}}:v1:{hash}")
    }

    /// 粘性映射的伴随键：该会话上一次**成功**请求的时刻（Unix 秒）。与映射同一个哈希标签，
    /// 单独成键而不是塞进映射的值里——回滚到旧版本时旧版仍能读懂映射本身，不会丢掉全站粘性。
    fn sess_at_key(user_id: i64, hash: &str) -> String {
        format!("stick:sess:{{{user_id}}}:v1:{hash}:at")
    }

    /// 读会话粘性映射并滑动续期（GET + EXPIRE 两步；续期非关键路径无需原子）。
    /// 一并带回上一次成功请求的时刻：额度准入据此判断提示缓存是否还热。
    pub async fn sticky_get(&self, user_id: i64, session_hash: &str) -> Option<StickyHit> {
        let key = Self::sess_key(user_id, session_hash);
        let at_key = Self::sess_at_key(user_id, session_hash);
        let pipe = self.client.pipeline();
        let _: () = pipe.get(&key).await.ok()?;
        let _: () = pipe.get(&at_key).await.ok()?;
        let values: Vec<Option<String>> = pipe.all().await.ok()?;
        let hit = StickyHit::parse(
            values.first()?.as_deref()?,
            values.get(1).and_then(|v| v.as_deref()),
        )?;
        // 两个键同寿：映射被读续期，时刻不能先过期，否则热会话会被当成「时刻未知」
        let _: Result<bool, _> = self.client.expire(&key, SESSION_TTL_SECS, None).await;
        let _: Result<bool, _> = self.client.expire(&at_key, SESSION_TTL_SECS, None).await;
        Some(hit)
    }

    /// 建立/刷新会话粘性映射（尽力而为：失败仅降低 cache 命中，不影响正确性）。
    pub async fn sticky_set(&self, user_id: i64, session_hash: &str, channel_key_id: i64) {
        let now = chrono::Utc::now().timestamp();
        let pipe = self.client.pipeline();
        let queued = async {
            let _: () = pipe
                .set(
                    Self::sess_key(user_id, session_hash),
                    channel_key_id.to_string(),
                    Some(Expiration::EX(SESSION_TTL_SECS)),
                    None,
                    false,
                )
                .await?;
            pipe.set(
                Self::sess_at_key(user_id, session_hash),
                now.to_string(),
                Some(Expiration::EX(SESSION_TTL_SECS)),
                None,
                false,
            )
            .await
        };
        let result: Result<(), fred::error::Error> = match queued.await {
            Ok(()) => pipe.all::<Vec<String>>().await.map(|_| ()),
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            tracing::debug!(error = %err, "sticky_set 失败（忽略）");
        }
    }
}

#[cfg(test)]
mod sticky_hit_tests {
    use super::StickyHit;

    #[test]
    fn mappings_parse_with_or_without_the_clock() {
        assert_eq!(
            StickyHit::parse("7", Some("1800000000")),
            Some(StickyHit {
                key: 7,
                last_used: Some(1_800_000_000)
            })
        );
        // 升级前写的映射没有伴随键；伴随键读不懂就当没有，不当作映射坏了
        for clock in [None, Some("garbage"), Some("")] {
            assert_eq!(
                StickyHit::parse("7", clock),
                Some(StickyHit {
                    key: 7,
                    last_used: None
                })
            );
        }
        assert_eq!(StickyHit::parse("not-a-key", Some("1")), None);
    }

    #[test]
    fn warm_means_served_within_the_window_and_unknown_counts_as_warm() {
        let hit = |last_used| StickyHit { key: 1, last_used };
        assert!(hit(Some(1_000)).is_warm(1_270, 270));
        assert!(!hit(Some(1_000)).is_warm(1_271, 270));
        assert!(hit(None).is_warm(9_999_999, 270), "旧映射保持此前行为");
        // 时钟回拨（上一次成功比现在还晚）不会溢出，也算热
        assert!(hit(Some(2_000)).is_warm(1_000, 270));
    }
}
