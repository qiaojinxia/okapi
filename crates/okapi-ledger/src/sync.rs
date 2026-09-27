//! PG-first completion for ordinary requests. No actual charge may disappear
//! between Redis closure and the durable bill. UserGuard also fences repairs,
//! subscription windows and expiry while a completion is being persisted.
use crate::{BalanceLedger, CommitOutcome, LedgerError, SettlementInput, holds::UserGuard};
use okapi_domain::{BillingState, Money};
use sqlx::{Connection, PgPool};

/// Returns true only for the first durable bill, including when Redis still needs
/// recovery. Callers may account best-effort statistics once from this result.
pub async fn record(
    pg: &PgPool,
    ledger: &BalanceLedger,
    mut input: SettlementInput<'_>,
) -> Result<bool, LedgerError> {
    if input.state != BillingState::Committed
        || input.log_type != 2
        || input.event_type != "commit"
        || !(0..=crate::holds::MAXIMUM_MICROS).contains(&input.amount.as_micros())
        || input.delta_micro != input.amount.as_micros().saturating_neg()
    {
        return Err(LedgerError::InvalidSettlement);
    }
    let mut guard = UserGuard::acquire(pg, input.user_id).await?;
    // The post-Redis snapshot is not known yet. Do not invent it in the PG event.
    input.balance_after = None;
    let mut tx = guard.connection().begin().await?;
    let inserted = crate::pg::record_settlement_in_tx(&mut tx, input.clone()).await?;
    if inserted {
        sqlx::query!(
            "INSERT INTO billing_sync (request_id,user_id,api_key_id,amount_micro,pool) VALUES ($1,$2,$3,$4,$5)",
            input.request_id, input.user_id, input.api_key_id, input.amount.as_micros(), input.pool.as_i16()
        ).execute(&mut *tx).await?;
    } else {
        let existing = sqlx::query!(
            r#"SELECT user_id AS "user_id!",api_key_id,amount_micro AS "amount_micro!",pool AS "pool!",status AS "status!" FROM billing_financial_records WHERE request_id=$1 ORDER BY created_at DESC LIMIT 1"#,
            input.request_id
        ).fetch_one(&mut *tx).await?;
        if existing.user_id != input.user_id
            || existing.api_key_id != Some(input.api_key_id)
            || existing.amount_micro != input.amount.as_micros()
            || existing.pool != input.pool.as_i16()
            || existing.status != BillingState::Committed.as_i16()
        {
            return Err(LedgerError::ReservationConflict);
        }
    }
    tx.commit().await?;
    if let Err(error) = guard.synchronize(ledger).await {
        tracing::error!(request_id=%input.request_id, %error, "durable bill awaiting Redis recovery");
    }
    Ok(inserted)
}

/// Finish pending Redis changes before reading totals for repair or changing a
/// subscription window. Even an ambiguous Redis/PG acknowledgement is retryable.
pub(crate) async fn synchronize(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    user_id: i64,
) -> Result<(), LedgerError> {
    let pending = sqlx::query!(
        "SELECT request_id,api_key_id,amount_micro,pool FROM billing_sync WHERE user_id=$1 ORDER BY created_at,request_id",
        user_id
    ).fetch_all(guard.connection()).await?;
    if pending.is_empty() {
        return Ok(());
    }
    let mut missing = false;
    for row in &pending {
        match ledger
            .commit_in_pool(
                user_id,
                row.api_key_id,
                row.request_id,
                Money::from_micros(row.amount_micro),
                crate::Pool::from_i16(row.pool),
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
        let totals = okapi_store::history::totals(guard.connection(), user_id).await?;
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
    .execute(guard.connection())
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
        let mut guard = UserGuard::acquire(pg, user_id).await?;
        match guard.synchronize(ledger).await {
            Ok(()) => recovered += 1,
            Err(error) => tracing::error!(user_id, %error, "ordinary settlement recovery deferred"),
        }
    }
    Ok(recovered)
}
