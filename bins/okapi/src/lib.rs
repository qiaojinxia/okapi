//! okapi 库目标：角色装配模块（bin 入口与集成测试共用）。
#![recursion_limit = "256"]

pub mod console;
pub mod gateway;
pub mod kernel;
pub mod migrate;
pub mod worker;

// 这几个模块挪进了 kernel（§11.59）；原路径照旧可用（bin 入口与集成测试按 `okapi::config` 等引用）。
pub use kernel::{config, mail, margin, ops, shutdown};
