//! Retained server-tool statistics: the gateway bill of every ingress, JSON and SSE, must reach the portal and admin
//! tool reports across raw-log loss, price changes and refunds. Split out of `gateway_usage_modalities`, which these
//! cases pushed past the per-suite deadline of `scripts/test-isolated.py` on CI runners: each builds a fresh ClickHouse
//! database and runs the tool report query several times. Planning that query took about 2.6 s per call until the
//! raw-log recovery moved behind `view()`; it is about 0.8 s now.
#[path = "../../../crates/okapi-gateway/tests/support/published_pricing.rs"]
mod published_pricing;

#[allow(dead_code)] // shared harness: each suite uses a different subset
#[path = "support/usage_modalities.rs"]
mod usage_modalities;
use usage_modalities::*;

#[path = "support/server_tool_prices.rs"]
mod server_tool_prices;

#[path = "support/server_tool_gateway_statistics.rs"]
mod server_tool_gateway_statistics;
