//! okapi-store：持久化层（PG 连接与迁移、Redis 客户端、按表组织的读写仓储与开发种子）。
//!
//! schema 唯一权威见 docs/database.md。这里只放必须和写入同处一个事务的规则
//! （占用检查、软删时停用从属行、出口代理固定分配）；计费写路径在 okapi-ledger，
//! 定价编译在 okapi-pricing，请求期的业务判断在 bins/okapi。

pub mod admin;
pub mod api_key_secret;
pub mod auth;
pub mod ch;
#[path = "channel_usage/mod.rs"]
pub mod channel_usage;
pub mod channels;
pub mod credential;
pub mod delivery;
pub mod egress;
pub mod egress_traffic;
pub mod env_config;
pub mod error;
pub mod history;
pub mod identity;
pub mod image_batches;
pub mod image_tasks;
pub mod legacy_speech;
pub mod listing;
pub mod model_config;
pub mod mutate;
pub mod netmatch;
pub mod oauth_credentials;
pub mod payments;
pub mod pg;
pub mod pricing;
pub mod provision;
pub mod redis;
pub mod subscriptions;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod timezone;
pub mod vendor;

pub use auth::AuthedKey;
pub use ch::ChClient;
pub use channels::ChannelCandidate;
pub use error::StoreError;
pub use pg::{connect_pg, run_migrations};
pub use redis::connect_redis;
