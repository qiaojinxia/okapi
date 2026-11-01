//! 各角色共用、不依赖数据面的底层：错误类型、请求提取器、客户端识别、SSRF 校验、价簿装载、
//! 调度用 Redis 门面（IMPLEMENTATION §11.57），共用状态 `CoreState`、鉴权与在途量计数（§11.58），
//! 以及配置、下线跟踪、负毛利熔断表、发信、运维监控、后台任务策略与账本对账 / 修复（§11.59）。
//!
//! 规矩：这里不引用 gateway / console / worker 的任何东西；gateway、console、worker 都可以引用这里。
//! gateway 的 `AppState` = `CoreState` + 数据面的分组，`Deref` 到 `CoreState`。

pub mod auth;
pub mod clients;
pub mod config;
pub mod error;
pub mod extract;
pub mod inflight;
pub mod mail;
pub mod margin;
pub mod ops;
pub mod policies;
pub mod pricing_loader;
pub mod reconcile;
pub mod repair;
pub mod sched_redis;
pub mod security_headers;
pub mod shutdown;
pub mod ssrf;
pub mod state;
pub mod text;
