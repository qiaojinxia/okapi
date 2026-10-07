use super::{Hold, Status, UserGuard, db, hot};
use crate::{BalanceLedger, LedgerError, Pool, SettlementInput, pg::record_settlement_in_tx};
use chrono::{DateTime, Utc};
use okapi_domain::BillingState;
use serde_json::{Value, json};
use sqlx::{Acquire, PgPool};

fn validate(hold: &Hold, input: &SettlementInput<'_>) -> Result<(), LedgerError> {
    if hold.id != input.request_id
        || hold.user_id != input.user_id
        || hold.api_key_id != input.api_key_id
        || hold.model_name != input.model_name
        || hold.pricing_snapshot.get("group").and_then(Value::as_str) != Some(input.group_code)
        || hold.pricing_snapshot.get("epoch").and_then(Value::as_i64) != input.pricing_epoch
    {
        return Err(LedgerError::HoldConflict);
    }
    let mut actual = input
        .pricing_snapshot
        .clone()
        .ok_or(LedgerError::InvalidHold("pricing"))?;
    let mut frozen = hold.pricing_snapshot.clone();
    let units = actual
        .as_object_mut()
        .ok_or(LedgerError::InvalidHold("pricing"))?
        .remove("media_units");
    let maximum = frozen
        .as_object_mut()
        .ok_or(LedgerError::InvalidHold("pricing"))?
        .remove("media_units");
    if actual != frozen
        || match (&units, &maximum) {
            (None, None) => false,
            (Some(n), Some(max)) => n.as_u64().zip(max.as_u64()).is_none_or(|(n, max)| n > max),
            _ => true,
        }
    {
        return Err(LedgerError::HoldConflict);
    }
    if !(0..=hold.maximum_micro).contains(&input.amount.as_micros())
        || input.original.as_micros() < 0
        || input.list_price.as_micros() < 0
        || input.upstream_cost.is_some_and(|v| v.as_micros() < 0)
        || input
            .original
            .as_micros()
            .checked_sub(input.discount.as_micros())
            != Some(input.amount.as_micros())
    {
        return Err(LedgerError::InvalidHold("settlement_amount"));
    }
    match input.state {
        BillingState::Reserved => return Err(LedgerError::InvalidHold("settlement_state")),
        BillingState::Committed => {}
        BillingState::Failed | BillingState::Refunded => {
            if !input.amount.is_zero() {
                return Err(LedgerError::InvalidHold("settlement_state"));
            }
        }
    }
    Ok(())
}

fn receipt(input: &SettlementInput<'_>) -> Value {
    json!({"state":input.state,"log_type":input.log_type,"usage":input.usage,
        "amount":input.amount.as_micros(),"original":input.original.as_micros(),
        "discount":input.discount.as_micros(),"list_price":input.list_price.as_micros(),
        "upstream_cost":input.upstream_cost.map(okapi_domain::Money::as_micros),
        "pricing":input.pricing_snapshot,"dimensions":input.dimensions,
        "channel_id":input.channel_id,"channel_key_id":input.channel_key_id,
        "latency_ms":input.latency_ms,"ttft_ms":input.ttft_ms,"stream":input.is_stream,
        "retry_count":input.retry_count,"failover_count":input.failover_count,
        "upstream_status":input.upstream_status,"error_code":input.error_code,
        "upstream_request_id":input.upstream_request_id,"node":input.node,
        "sticky_layer":input.sticky_layer,"client_type":input.client_type,"client_ip":input.client_ip})
}

fn replay_receipt(mut value: Value) -> Value {
    // Receipts predating these additive fields mean "not reported", not a
    // different settlement. Preserve every explicit value and all financial fields.
    // Diagnostics are free-form trace data, never money: a retry that recaptured
    // them must not turn an idempotent replay into a HoldConflict.
    if let Some(dimensions) = value.get_mut("dimensions").and_then(Value::as_object_mut) {
        dimensions.remove("diagnostics");
    }
    if let Some(usage) = value.get_mut("usage").and_then(Value::as_object_mut) {
        for field in ["cache_read_reported", "cache_write_reported"] {
            usage.entry(field).or_insert(Value::Bool(false));
        }
    }
    value
}

/// Commit the authoritative bill before releasing funds. A lost Redis acknowledgement is
/// recovered by UserGuard::synchronize; callers retry with exactly the same settlement.
pub async fn settle(
    pg: &PgPool,
    ledger: &BalanceLedger,
    mut input: SettlementInput<'_>,
    now: DateTime<Utc>,
) -> Result<Hold, LedgerError> {
    let mut guard = UserGuard::acquire(pg, input.user_id).await?;
    guard.synchronize(ledger).await?;
    let mut hold = db::get(&mut guard, input.request_id).await?;
    validate(&hold, &input)?;
    let receipt = receipt(&input);
    match hold.status {
        Status::Closed => {
            return if hold.settlement.as_ref().is_some_and(|stored| {
                replay_receipt(stored.clone()) == replay_receipt(receipt.clone())
            }) {
                Ok(hold)
            } else {
                Err(LedgerError::HoldConflict)
            };
        }
        Status::Pending if input.state == BillingState::Committed => {
            return Err(LedgerError::InvalidHold("not_held"));
        }
        Status::Closing => return Err(LedgerError::HoldRecoveryRequired),
        Status::Pending | Status::Held => {}
    }
    let funded = if hold.status == Status::Pending {
        // Persist before Redis IO; loss of the seal acknowledgement must not allow re-entry.
        sqlx::query("UPDATE balance_holds SET cancel_requested=TRUE,updated_at=now() WHERE id=$1 AND user_id=$2 AND state='pending'")
            .bind(hold.id).bind(hold.user_id).execute(guard.connection()?).await?;
        if let Some(receipt) = hot::seal(ledger, &hold).await? {
            hold.pool = Some(receipt.pool);
            hold.source_window = (receipt.pool == 1).then_some(receipt.epoch);
            true
        } else {
            hold.pool = Some(0);
            hold.source_window = None;
            false
        }
    } else {
        hot::verify(ledger, &hold).await?;
        true
    };
    let remaining = if funded {
        hold.maximum_micro
            .checked_sub(input.amount.as_micros())
            .ok_or(LedgerError::InvalidHold("settlement_amount"))?
    } else {
        0
    };
    let pool = hold.pool()?;
    let credit = match pool {
        Pool::Wallet => remaining,
        Pool::Subscription => {
            let (epoch, until) = hot::current_window(ledger, hold.user_id).await?;
            if hold.source_window.as_deref() == Some(epoch.as_str()) && now.timestamp() < until {
                remaining
            } else {
                0
            }
        }
    };
    let expired = remaining
        .checked_sub(credit)
        .ok_or(LedgerError::InvalidHold("settlement_amount"))?;
    input.pool = pool;
    input.source_window.clone_from(&hold.source_window);
    input.delta_micro = input
        .amount
        .as_micros()
        .checked_neg()
        .ok_or(LedgerError::InvalidHold("settlement_amount"))?;
    input.balance_after = None;
    input.event_type = match input.state {
        BillingState::Committed => "commit",
        BillingState::Failed | BillingState::Refunded => "refund",
        BillingState::Reserved => return Err(LedgerError::InvalidHold("settlement_state")),
    };
    let actual = input.amount.as_micros();
    let mut tx = guard.connection()?.begin().await?;
    if !record_settlement_in_tx(&mut tx, input).await? {
        return Err(LedgerError::HoldConflict);
    }
    if hold.status == Status::Pending && funded {
        sqlx::query("INSERT INTO billing_events(user_id,request_id,event_type,delta_micro,payload,actor,pool) VALUES($1,$2,'reserve',0,$3,'system:batch',$4)")
            .bind(hold.user_id).bind(hold.id)
            .bind(json!({"hold_micro":hold.maximum_micro,"model":hold.model_name,"source_window":hold.source_window,"recovered_admission":true}))
            .bind(pool.as_i16()).execute(&mut *tx).await?;
    }
    if expired > 0 {
        sqlx::query("INSERT INTO billing_events(user_id,request_id,event_type,delta_micro,payload,actor,pool) VALUES($1,$2,'sub_expire',$3,$4,'system:batch',1)")
            .bind(hold.user_id).bind(hold.id)
            .bind(expired.checked_neg().ok_or(LedgerError::InvalidHold("settlement_amount"))?)
            .bind(json!({"reason":"hold_window_expired","source_window":hold.source_window,"unused_micro":expired}))
            .execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE balance_holds SET state='closing',actual_micro=$3,credit_micro=$4,settlement=$5,pool=$6,source_window=$7,updated_at=now() WHERE id=$1 AND user_id=$2 AND state IN ('pending','held')")
        .bind(hold.id).bind(hold.user_id).bind(actual).bind(credit).bind(receipt)
        .bind(pool.as_i16()).bind(&hold.source_window).execute(&mut *tx).await?;
    tx.commit().await?;
    guard.synchronize(ledger).await?;
    db::get(&mut guard, hold.id).await
}

#[cfg(test)]
mod replay_tests {
    use super::replay_receipt;
    use serde_json::json;

    #[test]
    fn replay_ignores_diagnostics_but_not_money() {
        let stored =
            json!({"amount": 5000, "dimensions": {"model": "m", "diagnostics": {"trace": "a"}}});
        let retried =
            json!({"amount": 5000, "dimensions": {"model": "m", "diagnostics": {"trace": "b"}}});
        assert_eq!(replay_receipt(stored.clone()), replay_receipt(retried));
        let other =
            json!({"amount": 5001, "dimensions": {"model": "m", "diagnostics": {"trace": "a"}}});
        assert_ne!(replay_receipt(stored), replay_receipt(other));
    }
}
