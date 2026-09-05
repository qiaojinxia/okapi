//! okapi-store：存储层薄封装（PG 连接与迁移、Redis 客户端、只读仓储与开发种子）。
//!
//! schema 唯一权威见 docs/database.md；本 crate 不承载业务规则，
//! 计费写路径在 okapi-ledger，定价编译在 okapi-pricing。

pub mod admin;
pub mod auth;
pub mod ch;
pub mod channels;
pub mod credential;
pub mod error;
pub mod identity;
pub mod listing;
pub mod mutate;
pub mod netmatch;
pub mod pg;
pub mod pricing;
pub mod provision;
pub mod redis;
pub mod vendor;

pub use auth::AuthedKey;
pub use ch::ChClient;
pub use channels::ChannelCandidate;
pub use error::StoreError;
pub use pg::{connect_pg, run_migrations};
pub use redis::connect_redis;
