//! okapi-pricing：定价域（DESIGN §3 倍率模型 v3）。
//!
//! 职责分层：
//! - **配置时编译**：[`book::compile`] 把 模型倍率/分组/用户专属/规则栈 编译为 [`PriceBook`]；
//! - **请求时求值**：[`engine::calculate`] 在 PriceBook 上做 O(1) 查表 + 纯整数乘加；
//! - **热更新**：[`handle::PriceBookHandle`]（ArcSwap，epoch 单调递增）。
//!
//! 红线：本 crate 禁浮点；倍率为 scale=1e6 定点（[`ratio::RatioFp`]），金额为
//! micro-USD（`okapi_domain::Money`）；最终取整语义 = 向下截断（与 new-api `int()` 对齐）。

pub mod book;
pub mod engine;
pub mod error;
pub mod handle;
pub mod model;
pub mod ratio;
pub mod rules;
pub mod snapshot;

pub use book::{GroupEntry, ModelEntry, OverrideEntry, OverrideSpec, PriceBook, PriceBookSource};
pub use engine::{CalcContext, Quote, calculate};
pub use error::{CompileError, PricingError};
pub use handle::PriceBookHandle;
pub use model::{PricingMode, Tier, TierTable};
pub use ratio::RatioFp;
pub use rules::{PricingRule, RuleKind, RuleScope};
pub use snapshot::{AppliedRule, PricingSnapshot};
