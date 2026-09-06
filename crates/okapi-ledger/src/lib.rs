//! okapi-ledger：余额账本。
//!
//! - Redis 热账本：reserve / commit / refund / repair / sub_set 五个 Lua 契约（docs/database.md §2.2），
//!   全部键同 `{uid}` hash-tag，Cluster 单槽原子；钱包与订阅池两池并列（IMPLEMENTATION §11.28）；
//! - PG 记账：billing_records + billing_events + outbox 同事务（IMPLEMENTATION §2.2 步骤 13）。
//!
//! 红线（.cursor/rules/billing-safety.mdc）：禁浮点、禁 panic 类调用、宁停不错账。

pub mod pg;
pub mod redis;
pub mod subscriptions;

mod error;

pub use error::LedgerError;
pub use pg::{SettlementInput, record_settlement, record_sub_event};
pub use redis::{
    BalanceLedger, CommitOutcome, LimitCaps, Pool, RefundOutcome, RepairOutcome, Reservation,
    ReserveOutcome, ReserveRequest, SubSetOutcome,
};
