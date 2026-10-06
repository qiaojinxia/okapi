//! okapi-store：存储层薄封装（PG 连接与迁移、Redis 客户端、只读仓储与开发种子）。
//!
//! schema 唯一权威见 docs/database.md；本 crate 不承载业务规则，
//! 计费写路径在 okapi-ledger，定价编译在 okapi-pricing。

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
