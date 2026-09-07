//! 负毛利自动熔断（IMPLEMENTATION §11.34）：三个角色共用的状态形状与 Redis 契约。
//!
//! - worker 每 5 分钟按 `settings.margin_breaker` 评估 CH 立方体，把亏钱的「分组 × 渠道」
//!   写进 HASH `mb:blocks`（docs/database.md §2.1）；
//! - gateway 进程缓存 10s 一次 `HGETALL`，`blocked` 且未到 `until` 的对从候选里摘掉；
//! - console 列出 / 解除（`lifted`：评估器在 `until` 前跳过该对）。
//!
//! 粒度为什么是分组 × 渠道而不是模型：成本 = 官方价 × 渠道系数，收入 = 官方价 × 分组倍率 × …，
//! 两者都随官方价缩放，"亏"是（分组, 渠道）对的结构性属性。全程整数（bp = 万分比）。

use fred::clients::Client;
use fred::interfaces::{HashesInterface, KeysInterface};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 熔断状态 HASH 键。
pub const BLOCKS_KEY: &str = "mb:blocks";
/// settings 键。
pub const SETTING_KEY: &str = "margin_breaker";

/// `settings.margin_breaker`（缺省关；数值全部夹到安全区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BreakerConfig {
    pub enabled: bool,
    /// 评估窗口（小时，1..=168）。
    pub window_hours: i64,
    /// 成本已知的样本数下限。
    pub min_requests: i64,
    /// 窗口内上游成本下限（micro）：亏得太少不值得动。
    pub min_cost_micro: i64,
    /// 毛利率阈值（万分比，-10000..=10000）：低于它即熔断；0 = 只在真亏钱时动。
    pub margin_bp: i64,
    /// 熔断持续时间（秒），到期放行一轮流量重新采样。
    pub cooldown_secs: i64,
    /// 管理员解除后的免评估时长（秒）。
    pub lift_secs: i64,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            window_hours: 24,
            min_requests: 20,
            min_cost_micro: 100_000,
            margin_bp: 0,
            cooldown_secs: 3600,
            lift_secs: 86_400,
        }
    }
}

impl BreakerConfig {
    /// 从 settings 值解析；缺键用缺省，越界夹取——配置错一位数不该把全站渠道摘光。
    #[must_use]
    pub fn from_setting(value: Option<&Value>) -> Self {
        let d = Self::default();
        let Some(v) = value else {
            return d;
        };
        let int = |key: &str, default: i64, lo: i64, hi: i64| {
            v.get(key)
                .and_then(Value::as_i64)
                .unwrap_or(default)
                .clamp(lo, hi)
        };
        Self {
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            window_hours: int("window_hours", d.window_hours, 1, 168),
            min_requests: int("min_requests", d.min_requests, 1, i64::MAX),
            min_cost_micro: int("min_cost_micro", d.min_cost_micro, 0, i64::MAX),
            margin_bp: int("margin_bp", d.margin_bp, -10_000, 10_000),
            cooldown_secs: int("cooldown_secs", d.cooldown_secs, 60, 30 * 86_400),
            lift_secs: int("lift_secs", d.lift_secs, 60, 365 * 86_400),
        }
    }

    /// 一对（分组, 渠道）在窗口内的成本已知样本是否触发熔断。
    /// `(amount − cost) × 10000 < amount × margin_bp`，全整数、i128 防溢出。
    #[must_use]
    pub fn trips(&self, requests: i64, amount_micro: i64, cost_micro: i64) -> bool {
        if requests < self.min_requests || cost_micro < self.min_cost_micro || cost_micro <= 0 {
            return false;
        }
        let margin = i128::from(amount_micro) - i128::from(cost_micro);
        margin * 10_000 < i128::from(amount_micro) * i128::from(self.margin_bp)
    }
}

/// 毛利率（万分比）；收入为 0 时按 -10000 记（全亏）。
#[must_use]
pub fn margin_bp(amount_micro: i64, cost_micro: i64) -> i64 {
    if amount_micro <= 0 {
        return -10_000;
    }
    let margin = i128::from(amount_micro) - i128::from(cost_micro);
    i64::try_from(margin * 10_000 / i128::from(amount_micro)).unwrap_or(i64::MIN)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockState {
    Blocked,
    Lifted,
}

/// HASH 字段值。`until` 之后该条目失效（worker 下一轮剪掉）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockEntry {
    pub state: BlockState,
    /// 首次熔断 / 解除时刻（unix 秒）。
    pub since: i64,
    pub until: i64,
    pub requests: i64,
    pub amount_micro: i64,
    pub cost_micro: i64,
    pub margin_bp: i64,
}

impl BlockEntry {
    /// 此刻是否对网关生效。
    #[must_use]
    pub fn blocks_at(&self, now: i64) -> bool {
        self.state == BlockState::Blocked && self.until > now
    }
}

/// HASH 字段名：`<group>|<channel_id>`（group_code 是 VARCHAR(32) 标识符，不含 `|`）。
#[must_use]
pub fn field(group: &str, channel_id: i64) -> String {
    format!("{group}|{channel_id}")
}

#[must_use]
pub fn parse_field(field: &str) -> Option<(&str, i64)> {
    let (group, id) = field.rsplit_once('|')?;
    Some((group, id.parse().ok()?))
}

/// 读整张表（解析失败的字段跳过）。Redis 故障返回 None，调用方自行决定 fail-open。
pub async fn load_blocks(client: &Client) -> Option<HashMap<String, BlockEntry>> {
    let raw: HashMap<String, String> = client.hgetall(BLOCKS_KEY).await.ok()?;
    Some(
        raw.into_iter()
            .filter_map(|(k, v)| serde_json::from_str::<BlockEntry>(&v).ok().map(|e| (k, e)))
            .collect(),
    )
}

pub async fn set_block(
    client: &Client,
    field: &str,
    entry: &BlockEntry,
) -> Result<(), fred::error::Error> {
    let value = serde_json::to_string(entry).unwrap_or_default();
    client
        .hset::<(), _, _>(BLOCKS_KEY, (field.to_owned(), value))
        .await
}

pub async fn remove_blocks(client: &Client, fields: &[String]) -> Result<(), fred::error::Error> {
    if fields.is_empty() {
        return Ok(());
    }
    client.hdel::<(), _, _>(BLOCKS_KEY, fields.to_vec()).await
}

pub async fn clear_blocks(client: &Client) -> Result<(), fred::error::Error> {
    client.del::<(), _>(BLOCKS_KEY).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_defaults_and_clamps() {
        let d = BreakerConfig::from_setting(None);
        assert!(!d.enabled);
        assert_eq!((d.window_hours, d.min_requests, d.margin_bp), (24, 20, 0));
        let c = BreakerConfig::from_setting(Some(&json!({
            "enabled": true, "window_hours": 999, "min_requests": 0,
            "margin_bp": -50000, "cooldown_secs": 1, "lift_secs": 5
        })));
        assert!(c.enabled);
        assert_eq!(c.window_hours, 168);
        assert_eq!(c.min_requests, 1);
        assert_eq!(c.margin_bp, -10_000);
        assert_eq!(c.cooldown_secs, 60);
        assert_eq!(c.lift_secs, 60);
    }

    #[test]
    fn trips_only_on_real_loss_with_enough_samples() {
        let cfg = BreakerConfig {
            enabled: true,
            min_requests: 20,
            min_cost_micro: 100_000,
            ..BreakerConfig::default()
        };
        // 25 笔，收 2500 付 7500：亏
        assert!(cfg.trips(25, 2_500, 7_500_000));
        // 样本不足
        assert!(!cfg.trips(19, 2_500, 7_500_000));
        // 成本太小不值得动
        assert!(!cfg.trips(25, 0, 50_000));
        // 收入 0 但成本够大：全亏，熔
        assert!(cfg.trips(25, 0, 200_000));
        // 赚钱不熔
        assert!(!cfg.trips(25, 10_000_000, 5_000_000));
        // 刚好打平：margin_bp=0 时 0 < 0 不成立，不熔
        assert!(!cfg.trips(25, 5_000_000, 5_000_000));
        // 允许亏 5%：亏 3% 不熔、亏 8% 熔
        let tolerant = BreakerConfig {
            margin_bp: -500,
            ..cfg
        };
        assert!(!tolerant.trips(25, 10_000_000, 10_300_000));
        assert!(tolerant.trips(25, 10_000_000, 10_800_000));
        // 要求至少 20% 毛利：15% 毛利也熔
        let strict = BreakerConfig {
            margin_bp: 2_000,
            ..cfg
        };
        assert!(strict.trips(25, 10_000_000, 8_500_000));
    }

    #[test]
    fn margin_bp_math_and_field_roundtrip() {
        assert_eq!(margin_bp(10_000_000, 7_500_000), 2_500);
        assert_eq!(margin_bp(10_000_000, 12_000_000), -2_000);
        assert_eq!(margin_bp(0, 1), -10_000);
        assert_eq!(parse_field(&field("vip", 42)), Some(("vip", 42)));
        assert_eq!(parse_field("no-separator"), None);
        let entry = BlockEntry {
            state: BlockState::Blocked,
            since: 100,
            until: 200,
            requests: 25,
            amount_micro: 1,
            cost_micro: 2,
            margin_bp: -10_000,
        };
        assert!(entry.blocks_at(150));
        assert!(!entry.blocks_at(200));
        let lifted = BlockEntry {
            state: BlockState::Lifted,
            ..entry
        };
        assert!(!lifted.blocks_at(150));
        assert_eq!(serde_json::to_value(&lifted).unwrap()["state"], "lifted");
    }
}
