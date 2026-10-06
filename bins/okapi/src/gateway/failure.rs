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
    error_code: String,
    armed: bool,
    trace: Option<super::diagnostics::Trace>,
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
            error_code: okapi_api::codes::UPSTREAM_ERROR.to_owned(),
            armed: true,
            trace: super::diagnostics::Trace::current(),
        }
    }
    pub fn channel(&mut self, candidate: &okapi_store::ChannelCandidate) {
        if let Some(trace) = &self.trace {
            trace.begin(
                candidate,
                candidate.upstream_model(&self.model),
                &self.dimensions.upstream_endpoint,
            );
        }
        self.channel = Some((candidate.channel_id, candidate.channel_key_id));
        self.dimensions.upstream_model = candidate.upstream_model(&self.model).to_owned();
    }
    pub fn disarm(&mut self) {
        self.armed = false;
    }
    /// 交给 `settle_success` 前解除；它返回错误时什么账都没落，重新挂上留失败痕。
    pub fn arm(&mut self) {
        self.armed = true;
    }
    pub fn settlement_failed(&mut self, error: &super::error::AppError) {
        self.arm();
        self.error(error);
    }
    pub fn error(&mut self, error: &super::error::AppError) {
        self.error_code.clone_from(&error.code);
    }
    pub fn upstream(&mut self, model: &str, channel: (i64, i64)) {
        self.channel = Some(channel);
        model.clone_into(&mut self.dimensions.upstream_model);
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
        let dimensions = self
            .dimensions
            .clone()
            .with_diagnostics(self.trace.as_ref().map(super::diagnostics::Trace::snapshot));
        let pool = self.pool;
        let window = self.window.clone();
        let channel = self.channel;
        let started = self.started;
        let error_code = self.error_code.clone();
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
                    error_code: Some(&error_code),
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
