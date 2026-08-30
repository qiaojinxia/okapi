//! 计费状态机：billing_records.status 的唯一转移逻辑。
//!
//! 值对齐 docs/database.md §1.5：10 reserved / 20 committed / 30 refunded / 40 failed。
//! 红线：转移 match 必须穷举，禁止 `_ =>` 兜底。

use serde::{Deserialize, Serialize};

/// 计费记录状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingState {
    /// 已预扣（在途）。
    Reserved,
    /// 已结算。
    Committed,
    /// 已退款（预扣全额释放，或结算后管理员退款）。
    Refunded,
    /// 失败（上游失败/空回复，不计费）。
    Failed,
}

/// 状态机事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingEvent {
    /// 结算（多退少补）。
    Commit,
    /// 退款（在途释放 或 管理员按日志退款）。
    Refund,
    /// 标记失败。
    Fail,
}

/// 非法状态转移。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid_billing_transition: {from:?} + {event:?}")]
pub struct InvalidTransition {
    pub from: BillingState,
    pub event: BillingEvent,
}

impl BillingState {
    /// 应用事件，返回新状态；非法转移返回错误（由 ledger 层决定幂等跳过或告警）。
    pub fn apply(self, event: BillingEvent) -> Result<Self, InvalidTransition> {
        use BillingEvent::{Commit, Fail, Refund};
        use BillingState::{Committed, Failed, Refunded, Reserved};

        match (self, event) {
            (Reserved, Commit) => Ok(Committed),
            // 在途退款；以及结算后管理员按日志退款（IMPLEMENTATION §5.3）。
            (Reserved | Committed, Refund) => Ok(Refunded),
            (Reserved, Fail) => Ok(Failed),
            (Committed, Commit | Fail) | (Refunded | Failed, Commit | Refund | Fail) => {
                Err(InvalidTransition { from: self, event })
            }
        }
    }

    /// 终态：不再接受任何事件。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Refunded | Self::Failed)
    }

    /// DB 存储值（docs/database.md §1.5）。
    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::Reserved => 10,
            Self::Committed => 20,
            Self::Refunded => 30,
            Self::Failed => 40,
        }
    }

    /// 从 DB 存储值恢复。
    #[must_use]
    pub const fn from_i16(value: i16) -> Option<Self> {
        match value {
            10 => Some(Self::Reserved),
            20 => Some(Self::Committed),
            30 => Some(Self::Refunded),
            40 => Some(Self::Failed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BillingEvent::{Commit, Fail, Refund};
    use super::BillingState::{Committed, Failed, Refunded, Reserved};
    use super::*;

    /// 4 状态 × 3 事件全矩阵穷举（M0 验收项）。
    #[test]
    fn transition_matrix_is_exhaustive() {
        let expected: [(BillingState, BillingEvent, Option<BillingState>); 12] = [
            (Reserved, Commit, Some(Committed)),
            (Reserved, Refund, Some(Refunded)),
            (Reserved, Fail, Some(Failed)),
            (Committed, Commit, None),
            (Committed, Refund, Some(Refunded)),
            (Committed, Fail, None),
            (Refunded, Commit, None),
            (Refunded, Refund, None),
            (Refunded, Fail, None),
            (Failed, Commit, None),
            (Failed, Refund, None),
            (Failed, Fail, None),
        ];
        for (from, event, want) in expected {
            let got = from.apply(event).ok();
            assert_eq!(got, want, "from={from:?} event={event:?}");
        }
    }

    #[test]
    fn db_roundtrip() {
        for state in [Reserved, Committed, Refunded, Failed] {
            assert_eq!(BillingState::from_i16(state.as_i16()), Some(state));
        }
        assert_eq!(BillingState::from_i16(99), None);
    }
}
