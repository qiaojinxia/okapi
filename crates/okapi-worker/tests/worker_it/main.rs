//! Consolidated integration test binary for `okapi-worker`.
#![allow(clippy::duplicate_mod)]

pub mod channel_balance;
pub mod channel_probe;
pub mod core_chain_regressions;
pub mod gateway_pricing_epoch;
pub mod statistics_timezone;
pub mod worker_ch;
pub mod worker_delivery;
pub mod worker_ha;
pub mod worker_m2;
pub mod worker_nats;
pub mod worker_notify;
