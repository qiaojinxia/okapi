use crate::{BalanceLedger, LedgerError, holds::UserGuard};
use okapi_domain::Money;
use sqlx::{Postgres, Transaction};

pub(super) async fn schedule(
    tx: &mut Transaction<'_, Postgres>,
    uid: i64,
) -> Result<(), LedgerError> {
    sqlx::query!(
        "INSERT INTO subscription_sync(user_id) VALUES($1) ON CONFLICT(user_id) DO UPDATE SET retry_after=NULL",
        uid
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Only caller-owned user locks may create these effects. The PG balance target
/// includes existing reservations; the Redis repair subtracts those still live.
pub(super) async fn reset(
    tx: &mut Transaction<'_, Postgres>,
    uid: i64,
    quota: Money,
    frozen: Money,
    event: &str,
    actor: &str,
    payload: serde_json::Value,
) -> Result<(), LedgerError> {
    let target = quota
        .checked_add(frozen)
        .ok_or(LedgerError::InvalidHold("subscription_amount"))?;
    if !(0..=crate::holds::MAXIMUM_MICROS).contains(&target.as_micros()) {
        return Err(LedgerError::InvalidHold("subscription_amount"));
    }
    let current = okapi_store::history::totals(tx, uid).await?;
    let delta = target
        .checked_sub(Money::from_micros(current.subscription))
        .ok_or(LedgerError::InvalidHold("subscription_amount"))?;
    sqlx::query!("INSERT INTO billing_events(user_id,event_type,delta_micro,payload,actor,pool) VALUES($1,$2,$3,$4,$5,1)",uid,event,delta.as_micros(),payload,actor)
        .execute(&mut **tx).await?;
    schedule(tx, uid).await
}

pub(crate) async fn synchronize(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    uid: i64,
) -> Result<(), LedgerError> {
    let pending = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM subscription_sync WHERE user_id=$1) AS "pending!""#,
        uid
    )
    .fetch_one(guard.connection())
    .await?;
    if !pending {
        return Ok(());
    }
    let totals = okapi_store::history::totals(guard.connection(), uid).await?;
    guard
        .repair(
            ledger,
            Money::from_micros(totals.wallet),
            Money::from_micros(totals.subscription),
        )
        .await?;
    sqlx::query!("DELETE FROM subscription_sync WHERE user_id=$1", uid)
        .execute(guard.connection())
        .await?;
    Ok(())
}
