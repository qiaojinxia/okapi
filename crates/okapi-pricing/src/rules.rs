//! 规则修饰器栈（pricing_rules 的领域表示，保留 ok-api 灵活性）。
//!
//! 解析顺序固定可审计（DESIGN §3.4）：类间 volume → time_based → discount → surge，
//! 类内按 priority 升序、rule_code 字典序。编译时排序，请求时线性求值。

use crate::engine::CalcContext;
use crate::ratio::RatioFp;
use okapi_domain::{GroupCode, ModelCode, UserId};

/// 星期掩码：bit N = 星期 N（0=周日 … 6=周六，UTC，与分钟窗同钟源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeekdayMask(u8);

impl WeekdayMask {
    /// 每天（缺省；对既有规则零影响）。
    pub const ALL: Self = Self(0b0111_1111);

    /// 从日列表构造（0–6）；含非法值或空列表 → None（空掩码=永不命中，
    /// 与 start==end 空分钟窗同理，属配置错误应在写入口拒绝）。
    #[must_use]
    pub fn from_days(days: &[u8]) -> Option<Self> {
        if days.is_empty() {
            return None;
        }
        let mut mask = 0u8;
        for &d in days {
            if d > 6 {
                return None;
            }
            mask |= 1 << d;
        }
        Some(Self(mask))
    }

    #[must_use]
    pub const fn contains(self, weekday: u8) -> bool {
        weekday <= 6 && self.0 & (1 << weekday) != 0
    }

    /// 快照/展示用：命中的日列表。
    #[must_use]
    pub fn days(self) -> Vec<u8> {
        (0..=6u8).filter(|d| self.contains(*d)).collect()
    }
}

/// unix 秒 → UTC 星期（0=周日）。epoch 1970-01-01 是周四（=4）。
/// 与 `local_minute_of_day` 同为 UTC 钟源——若将来引入站点时区偏移，
/// 两者必须一起加偏移（engine 单点改）。
#[must_use]
pub fn weekday_utc(now_unix: i64) -> u8 {
    u8::try_from((now_unix.div_euclid(86_400) + 4).rem_euclid(7)).unwrap_or(0)
}

/// 规则类型与触发条件参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleKind {
    /// 量级折扣：用户当月累计 tokens 与消费额分别 ≥ 各自阈值时生效（AND；
    /// 0 = 该轴不设。消费额轴服务"贵模型大客户用量少但付费多"，§11.5）。
    Volume {
        min_monthly_tokens: u64,
        min_monthly_spend_micro: u64,
    },
    /// 时段折扣：分钟窗口（支持跨零点回绕，如 1320..360 = 22:00–06:00）
    /// × 星期掩码（缺省每天）。两者同为 UTC 钟源。
    TimeBased {
        start_minute: u16,
        end_minute: u16,
        weekdays: WeekdayMask,
    },
    /// 无条件折扣/加价。
    Discount,
    /// 负载加价：网关本地负载判定（ctx.surge_active）。
    Surge,
}

impl RuleKind {
    /// 类间固定序。
    #[must_use]
    pub const fn order(&self) -> u8 {
        match self {
            Self::Volume { .. } => 0,
            Self::TimeBased { .. } => 1,
            Self::Discount => 2,
            Self::Surge => 3,
        }
    }

    /// 快照用类型标签（对齐 pricing_rules.rule_type）。
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Volume { .. } => "volume",
            Self::TimeBased { .. } => "time_based",
            Self::Discount => "discount",
            Self::Surge => "surge",
        }
    }
}

/// 作用域选择器：None = 不限。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleScope {
    pub groups: Option<Vec<GroupCode>>,
    pub models: Option<Vec<ModelCode>>,
    pub users: Option<Vec<UserId>>,
}

impl RuleScope {
    fn matches(&self, ctx: &CalcContext) -> bool {
        let group_ok = self
            .groups
            .as_ref()
            .is_none_or(|groups| groups.contains(&ctx.group));
        let model_ok = self
            .models
            .as_ref()
            .is_none_or(|models| models.contains(&ctx.model));
        let user_ok = self
            .users
            .as_ref()
            .is_none_or(|users| users.contains(&ctx.user));
        group_ok && model_ok && user_ok
    }
}

/// 多规则命中时的叠加语义（老 ok-api stacking_mode 的乘法版，§11.5）。
/// 三种模式是**桶内策略**，三桶结果合并后按编译期固定序统一施加：
/// 无脑连乘的失控场景（"双十一 8 折 × 新人 9 折 = 0.72"）由站长把两条活动
/// 都标 `BestForUser` 解决——用户只享受更优的那一条。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stacking {
    /// 与其他命中规则连乘（缺省，历史行为）。
    #[default]
    Stackable,
    /// 桶内独占：只留 priority 最大的一条（平手取 code 字典序小者，与渠道
    /// priority "数值大优先"同语义）。
    Exclusive,
    /// 桶内取对用户最有利（乘数最小）的一条（平手取 code 字典序小者）。
    BestForUser,
}

impl Stacking {
    /// 快照/参数标签（对齐 pricing_rules.params.stacking_mode）。
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Stackable => "stackable",
            Self::Exclusive => "exclusive",
            Self::BestForUser => "best_for_user",
        }
    }

    /// 参数值解析；未知值 → None（fail-closed，调用方拒绝该规则）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "stackable" => Some(Self::Stackable),
            "exclusive" => Some(Self::Exclusive),
            "best_for_user" => Some(Self::BestForUser),
            _ => None,
        }
    }
}

/// 一条定价规则（编译进 PriceBook 后按固定序存放）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingRule {
    pub code: String,
    pub kind: RuleKind,
    /// 命中时施加的乘数（写入 pricing_snapshot.rules）。
    pub multiplier: RatioFp,
    pub scope: RuleScope,
    pub priority: i32,
    /// 多命中叠加语义（缺省 Stackable = 历史行为）。
    pub stacking: Stacking,
    /// 生效窗口（unix 秒，含 from 不含 to）。None = 不限。
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
}

impl PricingRule {
    /// 本次请求是否命中该规则。
    #[must_use]
    pub fn applies(&self, ctx: &CalcContext) -> bool {
        if !self.scope.matches(ctx) {
            return false;
        }
        if self.valid_from.is_some_and(|from| ctx.now_unix < from) {
            return false;
        }
        if self.valid_to.is_some_and(|to| ctx.now_unix >= to) {
            return false;
        }
        match self.kind {
            RuleKind::Volume {
                min_monthly_tokens,
                min_monthly_spend_micro,
            } => {
                ctx.monthly_tokens >= min_monthly_tokens
                    && ctx.monthly_spend_micro >= min_monthly_spend_micro
            }
            RuleKind::TimeBased {
                start_minute,
                end_minute,
                weekdays,
            } => {
                weekdays.contains(weekday_utc(ctx.now_unix))
                    && minute_in_window(ctx.local_minute_of_day, start_minute, end_minute)
            }
            RuleKind::Discount => true,
            RuleKind::Surge => ctx.surge_active,
        }
    }
}

/// 分钟窗口判定，支持跨零点回绕；[start, end)。
fn minute_in_window(minute: u16, start: u16, end: u16) -> bool {
    if start <= end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minute_window_wraps_midnight() {
        // 22:00–06:00
        assert!(minute_in_window(1380, 1320, 360));
        assert!(minute_in_window(0, 1320, 360));
        assert!(minute_in_window(359, 1320, 360));
        assert!(!minute_in_window(360, 1320, 360));
        assert!(!minute_in_window(720, 1320, 360));
        // 普通窗口 09:00–18:00
        assert!(minute_in_window(600, 540, 1080));
        assert!(!minute_in_window(1200, 540, 1080));
    }

    /// 对拍 new-api rc.27 #6934（时间规则区间恒真导致倍率全天生效）：
    /// 我们的语义 start == end = 空窗口永不命中，绝不退化为全天折扣。
    #[test]
    fn equal_bounds_is_empty_window_not_always_true() {
        for minute in [0u16, 599, 600, 601, 1439] {
            assert!(
                !minute_in_window(minute, 600, 600),
                "start==end 必须为空窗，minute={minute}"
            );
        }
    }

    /// 星期推导锚点：epoch（1970-01-01）= 周四；2026-09-01 = 周二。
    #[test]
    fn weekday_utc_anchors() {
        assert_eq!(weekday_utc(0), 4, "epoch 是周四");
        assert_eq!(weekday_utc(86_400), 5, "epoch 次日周五");
        // 2026-09-01 00:00:00 UTC = 1788220800，周二
        assert_eq!(weekday_utc(1_788_220_800), 2);
        // 负时间戳不 panic（div_euclid/rem_euclid 语义）
        assert_eq!(weekday_utc(-86_400), 3, "epoch 前一日周三");
    }

    /// 掩码构造：非法日与空列表拒绝（空掩码=永不命中，属配置错误须在写入口拦下）。
    #[test]
    fn weekday_mask_construction_and_membership() {
        let weekend = WeekdayMask::from_days(&[0, 6]).unwrap();
        assert!(weekend.contains(0) && weekend.contains(6));
        assert!(!weekend.contains(1) && !weekend.contains(5));
        assert_eq!(weekend.days(), vec![0, 6]);
        assert!(WeekdayMask::from_days(&[7]).is_none(), "7 不是合法星期");
        assert!(WeekdayMask::from_days(&[]).is_none(), "空列表拒绝");
        for d in 0..=6u8 {
            assert!(WeekdayMask::ALL.contains(d), "ALL 必须含每一天");
        }
    }

    /// stacking 标签双向一致（参数写入与快照展示共用一套字面量）。
    #[test]
    fn stacking_parse_and_tag_roundtrip() {
        for mode in [
            Stacking::Stackable,
            Stacking::Exclusive,
            Stacking::BestForUser,
        ] {
            assert_eq!(Stacking::parse(mode.tag()), Some(mode));
        }
        assert_eq!(Stacking::parse("nonsense"), None, "未知值 fail-closed");
    }
}
