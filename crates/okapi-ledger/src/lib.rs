//! okapi-ledger：余额账本。
//!
//! - Redis 热账本：reserve / commit / refund 三个 Lua 契约（docs/database.md §2.2），
//!   全部键同 `{uid}` hash-tag，Cluster 单槽原子；
//! - PG 记账：billing_records + billing_events + outbox 同事务（IMPLEMENTATION §2.2 步骤 13）。
//!
//! 红线（.cursor/rules/billing-safety.mdc）：禁浮点、禁 panic 类调用、宁停不错账。

pub mod pg;
pub mod redis;

mod error;

pub use error::LedgerError;
pub use pg::{SettlementInput, record_settlement};
pub use redis::{
    BalanceLedger, CommitOutcome, LimitCaps, Reservation, ReserveOutcome, ReserveRequest,
};
