//! Consolidated integration test binary for `okapi-store`.
#![allow(clippy::duplicate_mod)]

pub mod auth_find_key;
pub mod channel_pools;
pub mod clickhouse_streaming_error;
pub mod migrations;
pub mod model_config;
pub mod refund_measurements;
pub mod request_population;
pub mod schema_shape;
pub mod server_tool_history;
pub mod store_mutations;
