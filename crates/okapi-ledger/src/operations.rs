/// Serialize money operations and persist credit/refund recovery before Redis IO.
use crate::{BalanceLedger, LedgerError, holds::UserGuard, pg};
use okapi_domain::Money;
use serde_json::Value;
use sqlx::{Connection, PgPool};
use uuid::Uuid;

/// Accept the event, snapshot and recovery intent in one PG transaction. A
/// transient Redis failure returns a pending receipt and is recovered by worker.
pub async fn credit(
    db: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
    amount: Money,
    event_type: &str,
    actor: &str,
    payload: Value,
) -> Result<crate::transfers::Receipt, LedgerError> {
    let mut guard = UserGuard::acquire(db, user_id).await?;
    let mut tx = guard.connection()?.begin().await?;
    let id = crate::transfers::credit_in_tx(&mut tx, user_id, amount, event_type, actor, payload)
        .await?;
    tx.commit().await?;
    Ok(crate::transfers::finish(&mut guard, ledger, user_id, id, crate::Pool::Wallet).await)
}

/// Administrative clawback: drain the hot balance by `amount` (positive), capped
/// at the currently available funds — same clamp as `expire`. The event carries
/// the **negative** amount, so downstream cash-flow aggregation sees one adjust
/// stream. The negative fund transfer reuses the durable recovery intent.
/// Returns the applied amount (smaller than requested when the balance is short)
/// and the receipt; no PG event is written when there is nothing to drain.
pub async fn debit(
    db: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
    amount: Money,
    event_type: &str,
    actor: &str,
    payload: Value,
) -> Result<(Money, Option<crate::transfers::Receipt>), LedgerError> {
    let mut guard = UserGuard::acquire(db, user_id).await?;
    guard.synchronize(ledger).await?;
    let available = ledger.balance(user_id).await?.as_micros().max(0);
    let applied = amount.as_micros().min(available);
    if applied == 0 {
        return Ok((Money::ZERO, None));
    }
    let applied = Money::from_micros(applied);
    let mut tx = guard.connection()?.begin().await?;
    pg::record_credit_in_tx(
        &mut tx,
        user_id,
        Money::from_micros(applied.as_micros().saturating_neg()),
        event_type,
        actor,
        payload,
    )
    .await?;
    let id = crate::transfers::enqueue(
        &mut tx,
        user_id,
        Money::from_micros(-applied.as_micros()),
        crate::Pool::Wallet,
    )
    .await?;
    tx.commit().await?;
    let receipt =
        crate::transfers::finish(&mut guard, ledger, user_id, id, crate::Pool::Wallet).await;
    Ok((applied, Some(receipt)))
}

/// Refund status, bill/event updates and recovery intent are one PG commit.
pub async fn refund(
    db: &PgPool,
    ledger: &BalanceLedger,
    request_id: Uuid,
    reason: &str,
    actor: &str,
) -> Result<Option<(pg::AdminRefund, crate::transfers::Receipt)>, LedgerError> {
    let mut history = okapi_store::history::read(db).await?;
    let user_id = sqlx::query_scalar!(
        r#"SELECT user_id AS "user_id!" FROM billing_financial_records WHERE request_id=$1 ORDER BY created_at DESC LIMIT 1"#,
        request_id
    )
    .fetch_optional(&mut *history)
    .await?;
    history.commit().await?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    let mut guard = UserGuard::acquire(db, user_id).await?;
    guard.synchronize(ledger).await?;
    let mut tx = guard.connection()?.begin().await?;
    let result = pg::admin_refund_in_tx(&mut tx, request_id, reason, actor).await?;
    let Some(refund) = result else {
        tx.commit().await?;
        return Ok(None);
    };
    let id = crate::transfers::enqueue(&mut tx, user_id, refund.credit, refund.pool).await?;
    tx.commit().await?;
    let receipt = crate::transfers::finish(&mut guard, ledger, user_id, id, refund.pool).await;
    Ok(Some((refund, receipt)))
}

/// Migration imports one balance per user/source actor. Check its marker under
/// the same user lock, including PG-only offline imports.
pub async fn import_credit(
    db: &PgPool,
    ledger: Option<&BalanceLedger>,
    user_id: i64,
    amount: Money,
    actor: &str,
    payload: Value,
) -> Result<(), LedgerError> {
    let mut guard = UserGuard::acquire(db, user_id).await?;
    if let Some(ledger) = ledger {
        guard.synchronize(ledger).await?;
    }
    let mut tx = guard.connection()?.begin().await?;
    okapi_store::history::read_lock(&mut tx).await?;
    let already = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM billing_actor_totals WHERE user_id=$1 AND actor=$2) AS "exists!""#,
        user_id,
        actor
    )
    .fetch_one(&mut *tx)
    .await?;
    if already {
        tx.commit().await?;
        return Ok(());
    }
    pg::record_credit_in_tx(&mut tx, user_id, amount, "adjust", actor, payload).await?;
    let id = if ledger.is_some() {
        Some(crate::transfers::enqueue(&mut tx, user_id, amount, crate::Pool::Wallet).await?)
    } else {
        None
    };
    tx.commit().await?;
    if let (Some(ledger), Some(id)) = (ledger, id) {
        crate::transfers::finish(&mut guard, ledger, user_id, id, crate::Pool::Wallet).await;
    }
    Ok(())
}

/// Recheck expiry after acquiring the guard: another worker may have drained the
/// balance or an administrator may have extended its validity while we waited.
pub async fn expire(
    db: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Money, LedgerError> {
    let mut guard = UserGuard::acquire(db, user_id).await?;
    guard.synchronize(ledger).await?;
    let mut tx = guard.connection()?.begin().await?;
    let due = sqlx::query_scalar!(
        "SELECT id FROM users WHERE id=$1 AND balance_expires_at < $2 AND deleted_at IS NULL FOR UPDATE",
        user_id,
        now
    )
    .fetch_optional(&mut *tx)
    .await?;
    if due.is_none() {
        return Ok(Money::ZERO);
    }
    // Available funds only: in-flight charges/holds remain backed by the ledger.
    let drained = Money::from_micros(ledger.balance(user_id).await?.as_micros().max(0));
    if !drained.is_zero() {
        pg::record_credit_in_tx(
            &mut tx,
            user_id,
            Money::from_micros(drained.as_micros().saturating_neg()),
            "expire",
            "system:worker",
            serde_json::json!({"reason": "balance_expired"}),
        )
        .await?;
    }
    sqlx::query!(
        "UPDATE users SET balance_expires_at = NULL, updated_at = now() WHERE id = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    let transfer = if drained.is_zero() {
        None
    } else {
        Some(
            crate::transfers::enqueue(
                &mut tx,
                user_id,
                Money::from_micros(-drained.as_micros()),
                crate::Pool::Wallet,
            )
            .await?,
        )
    };
    tx.commit().await?;
    if let Some(id) = transfer {
        crate::transfers::finish(&mut guard, ledger, user_id, id, crate::Pool::Wallet).await;
    }
    Ok(drained)
}
