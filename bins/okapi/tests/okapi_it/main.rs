//! Consolidated integration test binary for `okapi`.
#![allow(clippy::duplicate_mod)]

pub mod audit_regressions;
pub mod billing_surface_parity;
pub mod channel_controls;
pub mod console_analytics;
pub mod console_logs;
pub mod console_ops;
pub mod console_pay;
pub mod console_portal;
pub mod console_redemption;
pub mod console_smtp;
pub mod console_stats;
pub mod console_subscriptions;
pub mod console_users;
pub mod egress_proxies;
pub mod gateway_audio;
pub mod gateway_realtime;
pub mod gateway_reserve_atomicity;
pub mod gateway_server_tool_billing;
pub mod gateway_server_tool_statistics;
pub mod gateway_shutdown;
pub mod gateway_usage_modalities;
pub mod migrate_newapi;
pub mod migrate_okapi_old;
pub mod request_population;
pub mod worker_margin_breaker;
pub mod worker_reconcile_repair;
pub mod worker_retention;
