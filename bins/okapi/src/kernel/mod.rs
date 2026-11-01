//! 各角色共用、不依赖数据面的底层：错误类型、请求提取器、客户端识别、SSRF 校验、价簿装载、
//! 调度用 Redis 门面（IMPLEMENTATION §11.57）。
//!
//! 规矩：这里不引用 gateway / console / worker 的任何东西；gateway、console、worker 都可以引用这里。
//! `AppState` 与鉴权（`gateway::state` / `gateway::auth`）还在 gateway：AppState 的字段与方法挂着
//! 数据面的类型和逻辑（上游客户端、结算重试、额度预判……），拆开它之后再挪过来。

pub mod clients;
pub mod error;
pub mod extract;
pub mod pricing_loader;
pub mod sched_redis;
pub mod ssrf;
