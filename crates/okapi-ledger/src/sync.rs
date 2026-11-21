//! PG-first completion for ordinary requests. No actual charge may disappear
//! between Redis closure and the durable bill. UserGuard also fences repairs,
//! subscription windows and expiry while a completion is being persisted.
mod batch;
pub use batch::{BatchOutcome, record_batch};

use crate::{BalanceLedger, CommitOutcome, LedgerError, SettlementInput, holds::UserGuard};
use okapi_domain::{BillingState, Money};
use sqlx::{Connection, PgPool};

/// Returns true only for the first durable bill, including when Redis still needs
/// recovery. Callers may account best-effort statistics once from this result.
pub async fn record(
    pg: &PgPool,
    ledger: &BalanceLedger,
    input: SettlementInput<'_>,
) -> Result<bool, LedgerError> {
    let mut guard = UserGuard::acquire(pg, input.user_id).await?;
    let mut tx = guard.connection()?.begin().await?;
    let (write, expired) = record_in_tx_detailed(&mut tx, input.clone()).await?;
    tx.commit().await?;
    let sync_result = if write.inserted && !write.has_other_pending {
        // Fast path: this settlement is the sole pending recovery entry for this user,
        // so close its Redis receipt directly and delete its billing_sync row without
        // re-probing all four recovery queues or re-reading billing_sync.
        finish_single_sync(&mut guard, ledger, &input).await
    } else {
        guard.synchronize(ledger).await
    };
    if sync_result.is_err() || write.has_other_pending || expired {
        let _ = ledger.invalidate_admission_fast_path(input.user_id).await;
    }
    if let Err(error) = sync_result {
        tracing::error!(request_id=%input.request_id, %error, "durable bill awaiting Redis recovery");
    }
    Ok(write.inserted)
}

async fn finish_single_sync(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    input: &SettlementInput<'_>,
) -> Result<(), LedgerError> {
    match ledger
        .commit_in_source(
            input.user_id,
            input.api_key_id,
            input.request_id,
            input.amount,
            input.pool,
            input.source_window.as_deref(),
        )
        .await?
    {
        CommitOutcome::Committed { pool, .. } if pool == input.pool => {
            sqlx::query("DELETE FROM billing_sync WHERE user_id = $1 AND request_id = $2")
                .bind(input.user_id)
                .bind(input.request_id)
                .execute(guard.connection()?)
                .await?;
            Ok(())
        }
        CommitOutcome::Committed { .. } => Err(LedgerError::ReservationConflict),
        CommitOutcome::NoReservation => guard.synchronize(ledger).await,
    }
}

/// Caller holds the user guard. A durable task result can share this transaction
/// with the exact ordinary charge, expiry revision and Redis recovery intent.
pub async fn record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: SettlementInput<'_>,
) -> Result<bool, LedgerError> {
    Ok(record_in_tx_detailed(tx, input).await?.0.inserted)
}

async fn record_in_tx_detailed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    mut input: SettlementInput<'_>,
) -> Result<(crate::pg::SettlementWrite, bool), LedgerError> {
    if input.state != BillingState::Committed
        || input.log_type != 2
        || input.event_type != "commit"
        || !(0..=crate::holds::MAXIMUM_MICROS).contains(&input.amount.as_micros())
        || input.delta_micro != input.amount.as_micros().saturating_neg()
    {
        return Err(LedgerError::InvalidSettlement);
    }
    // The post-Redis snapshot is not known yet. Do not invent it in the PG event.
    input.balance_after = None;
    let expired = crate::windows::replaced(
        tx,
        input.user_id,
        input.pool,
        input.source_window.as_deref(),
    )
    .await?;
    let write = crate::pg::record_settlement_with_sync_in_tx(tx, input.clone(), true).await?;
    if write.inserted {
        if expired && !input.amount.is_zero() {
            crate::windows::revise_expiry(
                tx,
                input.user_id,
                input.request_id,
                input.amount.as_micros(),
                input.source_window.as_deref(),
                "ordinary_window_settlement",
            )
            .await?;
        }
    } else {
        let existing = sqlx::query!(
            r#"SELECT user_id AS "user_id!",api_key_id,amount_micro AS "amount_micro!",pool AS "pool!",status AS "status!",source_window FROM billing_financial_records WHERE request_id=$1 ORDER BY created_at DESC LIMIT 1"#,
            input.request_id
        ).fetch_one(&mut **tx).await?;
        if existing.user_id != input.user_id
            || existing.api_key_id != Some(input.api_key_id)
            || existing.amount_micro != input.amount.as_micros()
            || existing.source_window != input.source_window
            || existing.pool != input.pool.as_i16()
            || existing.status != BillingState::Committed.as_i16()
        {
            return Err(LedgerError::ReservationConflict);
        }
    }
    Ok((write, expired))
}

/// Finish pending Redis changes before reading totals for repair or changing a
/// subscription window. Even an ambiguous Redis/PG acknowledgement is retryable.
pub(crate) async fn synchronize(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    user_id: i64,
) -> Result<(), LedgerError> {
    let pending = sqlx::query!(
        "SELECT request_id,api_key_id,amount_micro,pool,source_window FROM billing_sync WHERE user_id=$1 ORDER BY created_at,request_id",
        user_id
    ).fetch_all(guard.connection()?).await?;
    if pending.is_empty() {
        return Ok(());
    }
    let mut missing = false;
    for row in &pending {
        match ledger
            .commit_in_source(
                user_id,
                row.api_key_id,
                row.request_id,
                Money::from_micros(row.amount_micro),
                crate::Pool::try_from(row.pool)?,
                row.source_window.as_deref(),
            )
            .await?
        {
            CommitOutcome::Committed { pool, .. } if pool.as_i16() == row.pool => {}
            CommitOutcome::Committed { .. } => return Err(LedgerError::ReservationConflict),
            CommitOutcome::NoReservation => missing = true,
        }
    }
    if missing {
        // No receipt can mean an earlier successful close with a lost ACK, an
        // earlier expiry refund, or lost Redis data. Never guess and debit again.
        // All pending closes have now finished under the same user lock; rebuild
        // from live events plus carried history, retaining other reservations and holds.
        let totals = okapi_store::history::totals(guard.connection()?, user_id).await?;
        guard
            .repair(
                ledger,
                Money::from_micros(totals.wallet),
                Money::from_micros(totals.subscription),
            )
            .await?;
    }
    let ids: Vec<_> = pending.iter().map(|r| r.request_id).collect();
    sqlx::query!(
        "DELETE FROM billing_sync WHERE user_id=$1 AND request_id=ANY($2)",
        user_id,
        &ids
    )
    .execute(guard.connection()?)
    .await?;
    Ok(())
}

/// Bounded user batch, ordered by oldest outstanding completion. A damaged user
/// must not prevent unrelated users from recovering in the same worker tick.
pub async fn recover_pending(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
) -> Result<usize, LedgerError> {
    let users = sqlx::query_scalar!(
        "SELECT user_id FROM billing_sync GROUP BY user_id ORDER BY min(created_at),user_id LIMIT $1",
        limit.clamp(1, 1000)
    ).fetch_all(pg).await?;
    let mut recovered = 0;
    for user_id in users {
        let mut guard = match UserGuard::acquire(pg, user_id).await {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(user_id, %error, "settlement recovery lock deferred");
                continue;
            }
        };
        match guard.synchronize(ledger).await {
            Ok(()) => recovered += 1,
            Err(error) => tracing::error!(user_id, %error, "ordinary settlement recovery deferred"),
        }
    }
    Ok(recovered)
}
