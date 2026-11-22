//! 多窗口订阅额度「过期紧迫度（Expiry Urgency）」打分器（纯整数运算、零浮点、零 panic）。
//!
//! 解决单看 `next_reset` 在「5 小时短窗口 + 7 天周窗口」共存时的清仓盲区：
//! - 对账号的每个活跃窗口计算归一化待消耗压力：
//!   `burn_pressure(w) = remaining_pct(w) * window_duration_secs(w) / max(time_left_secs(w), 300)`
//! - 按窗口周期（短窗口 <= 24h vs 长窗口 > 24h）乘以对应清仓权重（周窗口缺省 3.5×，因为其总容量远大于 5h）；
//! - 跨窗口聚合为 `max_window + other_windows_sum / 4`（以最紧迫窗口为主导，双窗同时临期时自然加分）；
//! - 结合任务形状（长/短上下文）：大上下文冷请求（`prompt_tokens >= heavy_prompt_tokens`）要求
//!   账号所有活跃窗口的最小余量 `min_headroom >= heavy_min_headroom_pct`，避免刚写完大缓存就撞上限，
//!   快见底的临期账号则留给短请求刮干净最后一点余量。

use okapi_domain::pool_policy::ResolvedUrgencyPolicy;
use okapi_providers::account::quota::{Snapshot, Window};

/// 剩余时间下限（5 分钟）：防止距重置只剩几秒的窗口数值爆炸压死临期大块周额度。
const MIN_TIME_LEFT_SECS: i64 = 300;
/// 区分短窗口（如 5h）与长窗口（如 7d 周窗 / 30d 月窗）的时长边界（24 小时）。
const LONG_WINDOW_THRESHOLD_SECS: i64 = 86_400;
/// 缺失 `window_secs` 时的短/长窗口兜底时长（5h / 7d）。
const DEFAULT_SHORT_WINDOW_SECS: i64 = 5 * 3600;
const DEFAULT_LONG_WINDOW_SECS: i64 = 7 * 86_400;

/// 单条候选的多窗口过期紧迫度评分结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrgencyScore {
    /// 对本次请求形状是否安全（短请求恒 true；大上下文请求要求最小余量达到 `heavy_min_headroom_pct`）。
    pub heavy_safe: bool,
    /// 综合清仓紧迫度得分（越大越应优先消耗）。
    pub score: u64,
    /// 贡献最大紧迫度的主导窗口名称（如 `seven_day`、`five_hour`，供 Dry-Run 与诊断展示）。
    pub dominant_window: String,
    /// 全部活跃窗口中的最小剩余百分比（瓶颈余量）。
    pub min_headroom_pct: u8,
}

fn is_long_window(w: &Window, time_left_secs: i64) -> bool {
    if let Some(secs) = w.window_secs {
        return secs > LONG_WINDOW_THRESHOLD_SECS;
    }
    let lower = w.name.to_ascii_lowercase();
    lower.contains("week")
        || lower.contains("7d")
        || lower.contains("seven")
        || lower.contains("month")
        || lower.contains("30d")
        || time_left_secs > LONG_WINDOW_THRESHOLD_SECS
}

fn effective_duration_secs(w: &Window, long_window: bool) -> u128 {
    let secs = w.window_secs.unwrap_or(if long_window {
        DEFAULT_LONG_WINDOW_SECS
    } else {
        DEFAULT_SHORT_WINDOW_SECS
    });
    u128::try_from(secs.max(3600)).unwrap_or(3600)
}

/// 根据请求输入 Token 数与策略阈值，判断本次请求是否属于需要保护的大上下文冷任务。
#[must_use]
pub const fn is_heavy_request(prompt_tokens: u32, cfg: &ResolvedUrgencyPolicy) -> bool {
    cfg.heavy_prompt_tokens > 0 && prompt_tokens >= cfg.heavy_prompt_tokens
}

/// 对单份额度快照计算多窗口过期紧迫度；观测过期或无活跃重置窗口时返回 `None`（由调用方按层中位数插队）。
#[must_use]
pub fn score_snapshot(
    snapshot: &Snapshot,
    now: i64,
    cfg: &ResolvedUrgencyPolicy,
    heavy_request: bool,
) -> Option<UrgencyScore> {
    if !snapshot.fresh(now) {
        return None;
    }

    let mut max_weighted: u128 = 0;
    let mut sum_weighted: u128 = 0;
    let mut dominant_window: Option<&str> = None;
    let mut min_headroom: u8 = 100;
    let mut counted = 0usize;

    for w in &snapshot.windows {
        if w.used_percent > 100 {
            continue;
        }
        let Some(resets_at) = w.resets_at.filter(|&at| at > now) else {
            continue;
        };
        counted += 1;
        let remaining_pct = 100 - w.used_percent;
        min_headroom = min_headroom.min(remaining_pct);

        let raw_left = resets_at.saturating_sub(now);
        let time_left = u128::try_from(raw_left.max(MIN_TIME_LEFT_SECS)).unwrap_or(300);
        let long_window = is_long_window(w, raw_left);
        let duration = effective_duration_secs(w, long_window);
        let weight = u128::from(if long_window {
            cfg.long_window_weight
        } else {
            cfg.short_window_weight
        });

        // 归一化消耗压力（千分比精度）= remaining_pct * duration / time_left * weight / 1000
        let weighted = u128::from(remaining_pct)
            .saturating_mul(duration)
            .saturating_mul(weight)
            / time_left
            / 1_000;

        sum_weighted = sum_weighted.saturating_add(weighted);
        if dominant_window.is_none() || weighted > max_weighted {
            max_weighted = weighted;
            dominant_window = Some(w.name.as_str());
        }
    }

    if counted == 0 {
        return None;
    }

    let other_bonus = sum_weighted.saturating_sub(max_weighted) / 4;
    let combined = max_weighted.saturating_add(other_bonus);
    let score = u64::try_from(combined).unwrap_or(u64::MAX);
    let heavy_safe = !heavy_request || min_headroom >= cfg.heavy_min_headroom_pct;

    Some(UrgencyScore {
        heavy_safe,
        score,
        dominant_window: dominant_window.unwrap_or("unknown").to_owned(),
        min_headroom_pct: min_headroom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(now: i64, windows: Vec<Window>) -> Snapshot {
        Snapshot {
            observed_at: now,
            allowed: Some(true),
            windows,
            scoped: Vec::new(),
            threshold_window: None,
            plan: None,
        }
    }

    fn win(name: &str, used_percent: u8, window_secs: i64, left_secs: i64, now: i64) -> Window {
        Window {
            name: name.to_owned(),
            used_percent,
            resets_at: Some(now + left_secs),
            window_secs: Some(window_secs),
        }
    }

    #[test]
    fn expiring_weekly_quota_beats_nearly_empty_five_hour_window() {
        let now = 1_700_000_000;
        let cfg = ResolvedUrgencyPolicy::default();
        // 账号 A：5h 剩 30 分钟但仅剩 10% 余量；7d 刚刷新还剩 6 天
        let acct_a = snap(
            now,
            vec![
                win("five_hour", 90, 5 * 3600, 1800, now),
                win("seven_day", 5, 7 * 86_400, 6 * 86_400, now),
            ],
        );
        // 账号 B：5h 剩 2 小时（余量 80%）；7d 还剩 10 小时且还剩 60% 没用完！
        let acct_b = snap(
            now,
            vec![
                win("five_hour", 20, 5 * 3600, 2 * 3600, now),
                win("seven_day", 40, 7 * 86_400, 10 * 3600, now),
            ],
        );
        let sa = score_snapshot(&acct_a, now, &cfg, false).unwrap();
        let sb = score_snapshot(&acct_b, now, &cfg, false).unwrap();
        assert!(
            sb.score > sa.score,
            "临期大块周额度 B({}) 应远高于快用空的 5h 账号 A({})",
            sb.score,
            sa.score
        );
        assert_eq!(sb.dominant_window, "seven_day");
    }

    #[test]
    fn expiring_five_hour_window_with_high_headroom_beats_fresh_five_hour_window() {
        let now = 1_700_000_000;
        let cfg = ResolvedUrgencyPolicy::default();
        // 两个账号周窗口状态一致，但 C 的 5h 窗口 30 分钟后过期且剩 70%，D 刚刷新
        let acct_c = snap(
            now,
            vec![
                win("five_hour", 30, 5 * 3600, 1800, now),
                win("seven_day", 50, 7 * 86_400, 4 * 86_400, now),
            ],
        );
        let acct_d = snap(
            now,
            vec![
                win("five_hour", 30, 5 * 3600, 4 * 3600 + 1800, now),
                win("seven_day", 50, 7 * 86_400, 4 * 86_400, now),
            ],
        );
        let sc = score_snapshot(&acct_c, now, &cfg, false).unwrap();
        let sd = score_snapshot(&acct_d, now, &cfg, false).unwrap();
        assert!(sc.score > sd.score);
        assert_eq!(sc.dominant_window, "five_hour");
    }

    #[test]
    fn heavy_request_deprioritizes_low_headroom_expiring_account_while_short_request_finishes_it() {
        let now = 1_700_000_000;
        let cfg = ResolvedUrgencyPolicy::default();
        // 账号 G：5h 剩 10 分钟、只剩 8% 余量（低于 20% 门槛）
        let low = snap(
            now,
            vec![
                win("five_hour", 92, 5 * 3600, 600, now),
                win("seven_day", 40, 7 * 86_400, 3 * 86_400, now),
            ],
        );
        // 账号 H：余量充裕（50%）
        let healthy = snap(
            now,
            vec![
                win("five_hour", 50, 5 * 3600, 3600, now),
                win("seven_day", 50, 7 * 86_400, 3 * 86_400, now),
            ],
        );

        let short_low = score_snapshot(&low, now, &cfg, false).unwrap();
        assert!(short_low.heavy_safe, "短请求允许刮净临期尾量");

        let heavy_low = score_snapshot(&low, now, &cfg, true).unwrap();
        let heavy_healthy = score_snapshot(&healthy, now, &cfg, true).unwrap();
        assert!(
            !heavy_low.heavy_safe,
            "大上下文冷请求应避开余量低于门槛的账号"
        );
        assert!(heavy_healthy.heavy_safe);
    }
}
