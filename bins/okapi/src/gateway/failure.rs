//! A reserved media request must end with a charge or a durable failure log.
use super::state::AppState;
use okapi_domain::{BillingState, Money, TokenUsage};
use okapi_ledger::{Pool, SettlementInput};
use std::time::Instant;
use uuid::Uuid;

pub(crate) struct Guard {
    state: AppState,
    key: okapi_store::AuthedKey,
    id: Uuid,
    model: String,
    dimensions: okapi_ledger::pg::UsageDimensions,
    started: Instant,
    pool: Pool,
    window: Option<String>,
    channel: Option<(i64, i64)>,
    armed: bool,
}
impl Guard {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: &AppState,
        key: &okapi_store::AuthedKey,
        id: Uuid,
        model: &str,
        requested: &str,
        endpoint: &str,
        started: Instant,
        pool: Pool,
        window: Option<&str>,
    ) -> Self {
        Self {
            state: state.clone(),
            key: key.clone(),
            id,
            model: model.to_owned(),
            dimensions: okapi_ledger::pg::UsageDimensions::new(requested, "", endpoint, endpoint),
            started,
            pool,
            window: window.map(str::to_owned),
            channel: None,
            armed: true,
        }
    }
    pub fn channel(&mut self, candidate: &okapi_store::ChannelCandidate) {
        self.channel = Some((candidate.channel_id, candidate.channel_key_id));
        self.dimensions.upstream_model = candidate.upstream_model(&self.model).to_owned();
    }
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let state = self.state.clone();
        let key = self.key.clone();
        let id = self.id;
        let model = self.model.clone();
        let dimensions = self.dimensions.clone();
        let pool = self.pool;
        let window = self.window.clone();
        let channel = self.channel;
        let started = self.started;
        // Shutdown tracks this detached cleanup, including cancellation of the handler.
        self.state.settlements.spawn(async move {
            let _ = state.ledger.refund(key.user_id, key.key_id, id).await;
            state
                .settle_write(SettlementInput {
                    dimensions,
                    request_id: id,
                    log_type: 5,
                    user_id: key.user_id,
                    api_key_id: key.key_id,
                    group_code: &key.group_code,
                    model_name: &model,
                    channel_id: channel.map(|c| c.0),
                    channel_key_id: channel.map(|c| c.1),
                    state: BillingState::Failed,
                    usage: TokenUsage::default(),
                    amount: Money::ZERO,
                    original: Money::ZERO,
                    discount: Money::ZERO,
                    list_price: Money::ZERO,
                    upstream_cost: None,
                    pricing_epoch: None,
                    pricing_snapshot: None,
                    latency_ms: i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX),
                    ttft_ms: None,
                    is_stream: false,
                    retry_count: 0,
                    failover_count: 0,
                    upstream_status: None,
                    error_code: Some(okapi_api::codes::UPSTREAM_ERROR),
                    upstream_request_id: None,
                    node: &state.node,
                    sticky_layer: 0,
                    client_type: "",
                    client_ip: None,
                    delta_micro: 0,
                    balance_after: None,
                    event_type: "refund",
                    pool,
                    source_window: window,
                })
                .await;
        });
    }
}
