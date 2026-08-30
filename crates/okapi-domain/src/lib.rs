//! okapi-domain：计费领域基元。
//!
//! 红线（.cursor/rules/billing-safety.mdc）：本 crate 禁浮点、禁 panic 类调用；
//! 金额只能以 [`Money`]（i64 micro-USD）流动，算术必须显式 checked/saturating。

pub mod error;
pub mod ids;
pub mod money;
pub mod state;
pub mod tokens;

pub use error::DomainError;
pub use ids::{ApiKeyId, ChannelId, ChannelKeyId, GroupCode, ModelCode, UserId};
pub use money::Money;
pub use state::{BillingEvent, BillingState, InvalidTransition};
pub use tokens::TokenUsage;
