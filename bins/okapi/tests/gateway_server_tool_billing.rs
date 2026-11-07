//! Server-tool billing through the real gateway: capture of every ingress, JSON and SSE, published per-tool prices and
//! their admission and routing, Gemini native tools, native execution cost coverage, and the upstream cost and
//! reservation provenance of tool requests. Split out of `gateway_usage_modalities` for the per-suite deadline of
//! `scripts/test-isolated.py` on CI runners; both suites include the test-free harness instead of each other's cases.
#[path = "../../../crates/okapi-gateway/tests/support/published_pricing.rs"]
mod published_pricing;

#[allow(dead_code)] // shared harness: each suite uses a different subset
#[path = "support/usage_modalities.rs"]
mod usage_modalities;
use usage_modalities::*;

#[path = "support/server_tool_prices.rs"]
mod server_tool_prices;

/// The Anthropic usage cases run in `gateway_usage_modalities`; the modules here only need its fixture.
mod anthropic_usage {
    pub(super) use super::anthropic_fixture as fixture;
}

#[path = "support/server_tools.rs"]
mod server_tools;

#[path = "support/server_tool_fees.rs"]
mod server_tool_fees;

#[path = "support/server_tool_admission.rs"]
mod server_tool_admission;

#[path = "support/server_tool_scope.rs"]
mod server_tool_scope;

#[path = "support/server_tool_routing.rs"]
mod server_tool_routing;

#[path = "support/native_gemini_tools.rs"]
mod native_gemini_tools;

#[path = "support/server_tool_followups.rs"]
mod server_tool_followups;

#[path = "support/native_cost_coverage.rs"]
mod native_cost_coverage;

#[path = "support/cost_source.rs"]
mod cost_source;

#[path = "support/refund_cost_source.rs"]
mod refund_cost_source;

#[path = "support/reservation_provenance.rs"]
mod reservation_provenance;
