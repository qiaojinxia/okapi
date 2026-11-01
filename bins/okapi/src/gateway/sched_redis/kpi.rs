//! 实时 KPI：每秒请求 / tokens / 金额 / 错误四个序列，以及按 key 的限速计数快照。
use super::SchedulerRedis;
use fred::interfaces::{KeysInterface, LuaInterface};

impl SchedulerRedis {
    /// 平台实时 KPI 秒桶累加（docs/database.md §2.1 `kpi:*`）。
    ///
    /// 四个序列各自一个「每秒一键」的计数器，而非设计初稿的 ZSET 滑窗——
    /// ZSET 要为每笔请求存一个成员，10k RPS × 60s = 60 万成员常驻内存；
    /// 秒桶无论多大流量都只有 4 × 120 个小键，且读侧一条 MGET 取满窗口。
    ///
    /// 单条 Lua 完成四路累加 = 一次往返；EXPIRE 只在该秒首次写入时下发
    /// （`INCRBY` 返回值等于增量即首次，与 `crit_rate_incr` 同法）。
    /// 全程 fire-and-forget：**账本原子、统计尽力**（§2.2 末条），
    /// KPI 写失败不得影响结算。
    pub async fn kpi_record(&self, tokens: u64, amount_micro: i64, is_error: bool) {
        const LUA: &str = r"
            local ttl = tonumber(ARGV[1])
            for i = 1, 4 do
                local by = tonumber(ARGV[i + 1])
                if by > 0 and redis.call('INCRBY', KEYS[i], by) == by then
                    redis.call('EXPIRE', KEYS[i], ttl)
                end
            end
            return 1
        ";
        let sec = chrono::Utc::now().timestamp();
        let result: Result<i64, _> = self
            .client
            .eval(
                LUA,
                Self::kpi_keys(sec),
                vec![
                    KPI_TTL_SECS.to_string(),
                    "1".to_owned(),
                    i64::try_from(tokens).unwrap_or(i64::MAX).to_string(),
                    amount_micro.max(0).to_string(),
                    i64::from(is_error).to_string(),
                ],
            )
            .await;
        if let Err(err) = result {
            tracing::debug!(error = %err, "KPI 秒桶累加失败（忽略）");
        }
    }

    /// 读取最近 `window` 个**已完成**秒的 KPI 序列（旧→新）。
    ///
    /// 不含当前这一秒：它还在累加中，读进来会让每次刷新都看到一个偏低的尾点，
    /// 像是流量刚刚掉下去。窗口内四序列同 hash-tag，一条 MGET 取完。
    pub async fn kpi_window(&self, window: i64) -> Vec<KpiSecond> {
        let window = window.clamp(1, KPI_WINDOW_MAX);
        let latest = chrono::Utc::now().timestamp() - 1;
        let seconds: Vec<i64> = ((latest - window + 1)..=latest).collect();
        let keys: Vec<String> = seconds.iter().flat_map(|s| Self::kpi_keys(*s)).collect();
        let values: Vec<Option<i64>> = self.client.mget(keys).await.unwrap_or_default();

        seconds
            .iter()
            .enumerate()
            .map(|(idx, ts)| {
                let at = |offset: usize| {
                    values
                        .get(idx * KPI_SERIES + offset)
                        .copied()
                        .flatten()
                        .unwrap_or(0)
                };
                KpiSecond {
                    ts: *ts,
                    requests: at(0),
                    tokens: at(1),
                    amount_micro: at(2),
                    errors: at(3),
                }
            })
            .collect()
    }

    /// 读某把 key 的限速计数器当前值（本分钟 RPM/TPM、当日 RPD），
    /// 键形态与 reserve Lua 完全一致（docs/database.md §2.1 `rl:{uid}:k:*`）。
    ///
    /// 这是限流器**自己的视角**：RPM 计的是 reserve 通过的请求数、TPM 计的是预扣
    /// 估算 token——正因如此它才能回答"我离限流还有多远"，事后按 usage 算的
    /// 速率答不了这个问题。读失败一律 0（展示用途，不影响任何判定）。
    pub async fn key_rate_snapshot(&self, user_id: i64, key_id: i64) -> (i64, i64, i64) {
        let now = chrono::Utc::now();
        let minute = now.timestamp().div_euclid(60);
        let day = now.format("%Y%m%d");
        let keys = vec![
            format!("rl:{{{user_id}}}:k:{key_id}:rpm:{minute}"),
            format!("rl:{{{user_id}}}:k:{key_id}:tpm:{minute}"),
            format!("rl:{{{user_id}}}:k:{key_id}:rpd:{day}"),
        ];
        let values: Vec<Option<i64>> = self.client.mget(keys).await.unwrap_or_default();
        let at = |i: usize| values.get(i).copied().flatten().unwrap_or(0);
        (at(0), at(1), at(2))
    }

    /// 某一秒的四个序列键。`{kpi}` hash-tag 保证 Cluster 下同槽，
    /// 使跨序列跨秒的 MGET 成立（否则读窗口要退化成 N 次往返）。
    pub(super) fn kpi_keys(sec: i64) -> Vec<String> {
        ["req", "tok", "amt", "err"]
            .iter()
            .map(|series| format!("kpi:{{kpi}}:{series}:{sec}"))
            .collect()
    }
}

/// KPI 秒桶的序列数（req / tok / amt / err），MGET 结果按此步长切片。
const KPI_SERIES: usize = 4;
/// 秒桶存活时长：覆盖最大查询窗口 + 时钟偏移余量。
pub(super) const KPI_TTL_SECS: i64 = 360;
/// 实时窗口上限（秒）。超过这个跨度就该看 CH 聚合而非 Redis 秒桶。
pub const KPI_WINDOW_MAX: i64 = 300;

/// 一秒的平台 KPI 采样。
pub struct KpiSecond {
    pub ts: i64,
    pub requests: i64,
    pub tokens: i64,
    pub amount_micro: i64,
    pub errors: i64,
}
