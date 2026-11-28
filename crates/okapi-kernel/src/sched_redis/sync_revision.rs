//! 无 NATS 轻量配置同步（单调 revision 游标）。
//!
//! 任一副本修改渠道、模型、池策略、settings、负毛利熔断或发布新定价 epoch 时，
//! 对 Redis HASH `sync:revisions` 中对应主题执行原子 `HINCRBY 1`。
//! 各副本按轻周期 `HGETALL sync:revisions` 对比本地游标：
//! - 无 NATS 时作为主同步通道（默认 2s 收敛，消除 5–60s 进程内 TTL 滞后）；
//! - 有 NATS 时兼作低频安全网（默认 15s），对丢广播 / 短暂断流自动自愈。
//!
//! 单个 HASH 键在 Redis Cluster 下落同一槽位，无 CROSSSLOT 风险；零 PostgreSQL 轮询压力。

use super::SchedulerRedis;
use fred::interfaces::HashesInterface;
use std::collections::HashMap;

/// 存放各同步主题单调版本号的 Redis HASH 键。
pub const SYNC_REVISIONS_KEY: &str = "sync:revisions";

/// 定价 epoch 发布的 revision 主题名（其余主题复用 `CacheInvalidation::to_wire()`）。
pub const PRICING_REVISION_TOPIC: &str = "pricing";

impl SchedulerRedis {
    /// 递增指定同步主题的单调版本号，返回新版本；Redis 异常时降级忽略（由 TTL 兜底）。
    pub async fn sync_revision_bump(&self, topic: &str) -> Option<u64> {
        match self
            .client
            .hincrby::<i64, _, _>(SYNC_REVISIONS_KEY, topic, 1)
            .await
        {
            Ok(next) => u64::try_from(next).ok().filter(|&v| v > 0),
            Err(err) => {
                tracing::debug!(error = %err, topic, "配置 revision 递增失败（依赖 TTL 兜底）");
                None
            }
        }
    }

    /// 读取全量主题版本号快照（单次 `HGETALL` 往返）。
    pub async fn sync_revisions_snapshot(&self) -> Result<HashMap<String, u64>, fred::error::Error> {
        let raw: HashMap<String, String> = self.client.hgetall(SYNC_REVISIONS_KEY).await?;
        Ok(parse_revisions_map(raw))
    }
}

fn parse_revisions_map(raw: HashMap<String, String>) -> HashMap<String, u64> {
    raw.into_iter()
        .filter_map(|(topic, value)| {
            if topic.is_empty() {
                return None;
            }
            let rev = value.trim().parse::<u64>().ok()?;
            (rev > 0).then_some((topic, rev))
        })
        .collect()
}

/// 进程内单调 revision 游标：记录每个同步主题已生效的远端版本号。
///
/// 关键不变量：
/// 1. 启动时用首次快照初始化基线（[`Self::seed_if_unseeded`]），冷启动不误刷全量缓存；
///    若启动快照因 Redis 瞬断失败，调用方应标记 [`Self::mark_seeded_empty`]，避免恢复后把
///    期间其它副本写入的变更误当初始基线吞掉。
/// 2. 本副本刚执行完本地缓存失效并递增 Redis 版本后，通过 [`Self::observe_local_bump`]
///    仅在 `new_rev == prev + 1` 时前推游标——既避免本副本 2s 后重复刷一次缓存，又绝不会
///    跳过并发窗口内其它副本插入的递增（此时 `new_rev > prev + 1`，保留旧游标待下轮轮询同步）。
/// 3. Redis 重启或 `FLUSHDB` 后远端版本可能回退（`0 < remote < local`）或整键消失：
///    [`Self::diff`] 按 `remote != local` 判变更并清理已消失的键，确保自动恢复。
#[derive(Debug, Default)]
pub struct RevisionCursor {
    seeded: bool,
    revisions: HashMap<String, u64>,
}

impl RevisionCursor {
    #[must_use]
    pub fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// 首次启动以当前 Redis 快照建立基线，不触发失效。
    pub fn seed_if_unseeded(&mut self, snapshot: HashMap<String, u64>) {
        if !self.seeded {
            self.revisions = snapshot;
            self.seeded = true;
        }
    }

    /// 启动快照不可用时以空基线进入轮询态：Redis 恢复后所有非零主题都会触发一次补刷，不漏更。
    pub fn mark_seeded_empty(&mut self) {
        if !self.seeded {
            self.seeded = true;
        }
    }

    /// 本副本刚完成本地失效并把 Redis 版本推进到 `new_rev` 后调用，用于线性可证的自回声抑制。
    pub fn observe_local_bump(&mut self, topic: &str, new_rev: u64) {
        if !self.seeded || new_rev == 0 {
            return;
        }
        let prev = self.revisions.get(topic).copied().unwrap_or(0);
        if new_rev == prev.saturating_add(1) {
            self.revisions.insert(topic.to_owned(), new_rev);
        }
    }

    /// 将最新快照与本地游标对比，返回发生变更的有序主题列表并更新本地游标。
    pub fn diff(&mut self, snapshot: &HashMap<String, u64>) -> Vec<String> {
        if !self.seeded {
            self.seed_if_unseeded(snapshot.clone());
            return Vec::new();
        }

        // Redis 重启/清空导致某主题消失时，把本地记录归零，保证下次写入 1 能被立刻捕获。
        self.revisions
            .retain(|topic, _| snapshot.get(topic).copied().unwrap_or(0) > 0);

        let mut changed = Vec::new();
        for (topic, &remote_rev) in snapshot {
            if remote_rev == 0 {
                continue;
            }
            let local_rev = self.revisions.get(topic).copied().unwrap_or(0);
            if remote_rev != local_rev {
                changed.push(topic.clone());
                self.revisions.insert(topic.clone(), remote_rev);
            }
        }
        changed.sort();
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &[&str] = &[];

    fn map(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), *v))
            .collect()
    }

    #[test]
    fn seed_establishes_baseline_and_detects_subsequent_advances() {
        let mut cursor = RevisionCursor::default();
        assert!(!cursor.is_seeded());

        // 首次快照只建基线，不触发冷启动误刷
        cursor.seed_if_unseeded(map(&[("channels", 5), ("models", 2)]));
        assert!(cursor.is_seeded());
        assert_eq!(cursor.diff(&map(&[("channels", 5), ("models", 2)])), EMPTY);

        // 其它副本推进 channels 并新增 pricing 主题
        let changed = cursor.diff(&map(&[("channels", 6), ("models", 2), ("pricing", 1)]));
        assert_eq!(changed, vec!["channels", "pricing"]);

        // 再次拉到相同快照不再重复触发
        assert_eq!(
            cursor.diff(&map(&[("channels", 6), ("models", 2), ("pricing", 1)])),
            EMPTY
        );
    }

    #[test]
    fn local_bump_suppresses_self_echo_only_when_strictly_consecutive() {
        let mut cursor = RevisionCursor::default();
        cursor.seed_if_unseeded(map(&[("channels", 5)]));

        // 本副本把 channels 从 5 推到 6：严格连续，安全跳过自回声
        cursor.observe_local_bump("channels", 6);
        assert_eq!(cursor.diff(&map(&[("channels", 6)])), EMPTY);

        // 首次出现的新主题从 0 -> 1：同样严格连续
        cursor.observe_local_bump("setting:ssrf_policy", 1);
        assert_eq!(
            cursor.diff(&map(&[("channels", 6), ("setting:ssrf_policy", 1)])),
            EMPTY
        );

        // 若并发窗口内其它副本先推到 7、本副本推到 8：8 != 6 + 1，绝不吞掉 peer 变更
        cursor.observe_local_bump("channels", 8);
        assert_eq!(
            cursor.diff(&map(&[("channels", 8), ("setting:ssrf_policy", 1)])),
            vec!["channels"]
        );
    }

    #[test]
    fn redis_restart_and_flush_reset_are_detected() {
        let mut cursor = RevisionCursor::default();
        cursor.seed_if_unseeded(map(&[("channels", 12), ("pools", 4)]));

        // 场景 A：Redis 重启并很快有一次新写入（版本从 12 回退到 1）
        let changed = cursor.diff(&map(&[("channels", 1), ("pools", 4)]));
        assert_eq!(changed, vec!["channels"]);
        assert_eq!(cursor.diff(&map(&[("channels", 1), ("pools", 4)])), EMPTY);

        // 场景 B：Redis FLUSHDB 清空所有键，随后再次写入版本 1
        assert_eq!(cursor.diff(&map(&[])), EMPTY);
        assert_eq!(cursor.diff(&map(&[("pools", 1)])), vec!["pools"]);
    }

    #[test]
    fn empty_seed_after_boot_error_catches_changes_on_recovery() {
        let mut cursor = RevisionCursor::default();
        cursor.mark_seeded_empty();
        assert!(cursor.is_seeded());

        let changed = cursor.diff(&map(&[("models", 3), ("pricing", 2)]));
        assert_eq!(changed, vec!["models", "pricing"]);
    }

    #[test]
    fn parse_revisions_map_ignores_corrupted_and_zero_entries() {
        let raw = HashMap::from([
            ("channels".to_owned(), "42".to_owned()),
            ("models".to_owned(), " 7 ".to_owned()),
            ("zero".to_owned(), "0".to_owned()),
            ("negative".to_owned(), "-1".to_owned()),
            ("garbage".to_owned(), "abc".to_owned()),
            (String::new(), "5".to_owned()),
        ]);
        assert_eq!(
            parse_revisions_map(raw),
            map(&[("channels", 42), ("models", 7)])
        );
    }
}
