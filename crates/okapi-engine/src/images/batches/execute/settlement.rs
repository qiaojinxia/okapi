use super::{AppError, AppState, Batch, map_store, results, store};
use okapi_domain::{BillingState, Money};
use okapi_ledger::{Pool, SettlementInput, holds, pg::UsageDimensions};

pub(super) async fn settle(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
) -> Result<(), AppError> {
    let units = u32::try_from(row.success_count).map_err(|_| AppError::internal())?;
    let quote = store::UnitQuote::read(&row.unit_quote)
        .map_err(map_store)?
        .total(units)
        .map_err(map_store)?;
    let usage = results::total_usage(store::usage(&state.pg, lease).await.map_err(map_store)?)?;
    let mut pricing = row.pricing_snapshot.clone();
    pricing["media_units"] = serde_json::json!(units);
    // All receipt fields depend only on sealed results and immutable admission data.
    // Retry errors, current clocks, prices and worker hostnames cannot change settlement.
    let error = if units == 0 {
        Some("batch_failed")
    } else if row.failure_count > 0 {
        Some("batch_partial")
    } else {
        None
    };
    let input = SettlementInput {
        source_window: None,
        dimensions: UsageDimensions::new(
            &row.model_name,
            &row.upstream_model,
            "/v1/images/batches",
            if row.provider == "gemini" {
                "batchGenerateContent"
            } else {
                "batchPredictionJobs"
            },
        ),
        request_id: row.id,
        log_type: if units > 0 { 2 } else { 5 },
        user_id: row.user_id,
        api_key_id: row.api_key_id,
        group_code: &row.group_code,
        model_name: &row.model_name,
        channel_id: Some(row.channel_id),
        channel_key_id: Some(row.channel_key_id),
        state: if units > 0 {
            BillingState::Committed
        } else {
            BillingState::Refunded
        },
        usage,
        amount: Money::from_micros(quote.amount),
        original: Money::from_micros(quote.original),
        discount: Money::from_micros(quote.discount),
        list_price: Money::from_micros(quote.list_price),
        upstream_cost: quote.upstream_cost.map(Money::from_micros),
        pricing_epoch: row
            .pricing_snapshot
            .get("epoch")
            .and_then(serde_json::Value::as_i64),
        pricing_snapshot: Some(pricing),
        latency_ms: row.results_ready_at.map_or(0, |ready| {
            i32::try_from((ready - row.created_at).num_milliseconds().max(0)).unwrap_or(i32::MAX)
        }),
        ttft_ms: None,
        is_stream: false,
        retry_count: 0,
        failover_count: 0,
        upstream_status: row.provider_job_name.as_ref().map(|_| 200),
        error_code: error,
        upstream_request_id: row.provider_job_name.as_deref(),
        node: "batch",
        sticky_layer: 0,
        client_type: &row.client_type,
        client_ip: row.client_ip.as_deref(),
        delta_micro: 0,
        balance_after: None,
        event_type: "commit",
        pool: Pool::Wallet,
    };
    holds::settle(&state.pg, &state.ledger, input, chrono::Utc::now()).await?;
    store::finish(&state.pg, lease).await.map_err(map_store)?;
    // Publication/financial settlement remain successful if the statistics sink is down.
    // The durable delivery row is retried by the worker independently.
    if let Err(error) = super::statistics::run_statistics(state, Some(row.id)).await {
        tracing::warn!(batch_id=%row.id,?error,"batch statistics pending retry");
    }
    Ok(())
}
