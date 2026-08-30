//! 规则修饰器栈（pricing_rules 的领域表示，保留 ok-api 灵活性）。
//!
//! 解析顺序固定可审计（DESIGN §3.4）：类间 volume → time_based → discount → surge，
//! 类内按 priority 升序、rule_code 字典序。编译时排序，请求时线性求值。

use crate::engine::CalcContext;
use crate::ratio::RatioFp;
use okapi_domain::{GroupCode, ModelCode, UserId};

/// 规则类型与触发条件参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleKind {
    /// 量级折扣：用户当月累计 tokens ≥ 阈值时生效（读 Redis KPI 计数，ctx 传入）。
    Volume { min_monthly_tokens: u64 },
    /// 时段折扣：站点本地时区的分钟窗口（支持跨零点回绕，如 1320..360 = 22:00–06:00）。
    TimeBased { start_minute: u16, end_minute: u16 },
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

/// 一条定价规则（编译进 PriceBook 后按固定序存放）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingRule {
    pub code: String,
    pub kind: RuleKind,
    /// 命中时施加的乘数（写入 pricing_snapshot.rules）。
    pub multiplier: RatioFp,
    pub scope: RuleScope,
    pub priority: i32,
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
            RuleKind::Volume { min_monthly_tokens } => ctx.monthly_tokens >= min_monthly_tokens,
            RuleKind::TimeBased {
                start_minute,
                end_minute,
            } => minute_in_window(ctx.local_minute_of_day, start_minute, end_minute),
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
}
