//! okapi 库目标：把各角色 crate 汇到一处（bin 入口与集成测试共用，路径与拆分前一致）。
//!
//! 依赖方向（IMPLEMENTATION §11.60）：okapi-kernel ← okapi-gateway ← {okapi-console, okapi-worker}。

pub use okapi_console as console;
pub use okapi_gateway as gateway;
pub use okapi_kernel as kernel;
pub use okapi_worker as worker;

pub mod migrate;

// 原在 crate 根的共用模块（§11.59 挪进了 kernel）；原路径照旧可用。
pub use okapi_kernel::{config, mail, margin, ops, shutdown};
