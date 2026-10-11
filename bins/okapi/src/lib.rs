//! okapi 库目标：角色装配模块（bin 入口与集成测试共用）。
#![recursion_limit = "256"]

pub mod config;
pub mod console;
pub mod gateway;
pub mod mail;
pub mod margin;
pub mod migrate;
pub mod ops;
pub(crate) mod security_headers;
pub mod shutdown;
pub(crate) mod text;
pub mod worker;
