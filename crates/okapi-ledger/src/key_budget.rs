//! Lifetime key budget: settled spend + ordinary reservations + durable holds.
//! The same user guard fences admission, PG-first settlement and durable jobs.
use crate::{BalanceLedger, LedgerError, ReserveOutcome, ReserveRequest, holds::UserGuard};
use chrono::{DateTime, Utc};
use okapi_domain::Money;
use sqlx::PgPool;
use uuid::Uuid;

pub(crate) async fn check(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    user_id: i64,
    key_id: i64,
    amount: Money,
    exclude_hold: Option<Uuid>,
) -> Result<(), LedgerError> {
    let (mode, limit, spent): (i16, Option<i64>, i64) = sqlx::query_as(
        "SELECT quota_mode,quota_micro,used_micro FROM api_keys WHERE id=$1 AND user_id=$2 AND deleted_at IS NULL"
    ).bind(key_id).bind(user_id).fetch_optional(guard.connection()?).await?
        .ok_or(LedgerError::InvalidReservation)?;
    if mode != 1 {
        return Ok(());
    }
    if amount.as_micros() < 0 || spent < 0 {
        return Err(LedgerError::AdmissionStateInvalid);
    }
    let held: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(maximum_micro),0)::bigint FROM balance_holds WHERE api_key_id=$1 AND state IN ('pending','held') AND ($2::uuid IS NULL OR id<>$2)"
    ).bind(key_id).bind(exclude_hold).fetch_one(guard.connection()?).await?;
    let mut total = i128::from(spent) + i128::from(held) + i128::from(amount.as_micros());
    for reservation in ledger.list_reservations(user_id).await? {
        if reservation.api_key_id == 0 || reservation.amount.as_micros() < 0 {
            return Err(LedgerError::AdmissionStateInvalid);
        }
        if reservation.api_key_id == key_id {
            total += i128::from(reservation.amount.as_micros());
        }
    }
    let limit = limit.unwrap_or(0);
    if spent >= limit || total > i128::from(limit) {
        return Err(LedgerError::KeyQuotaExceeded);
    }
    Ok(())
}

impl BalanceLedger {
    /// All admissions share the settlement/expiry fence. Limited keys additionally
    /// check fresh spend; unlimited keys must not bypass the balance expiry lock.
    pub async fn reserve_for_key(
        &self,
        pg: &PgPool,
        limited: bool,
        request: ReserveRequest,
        now: DateTime<Utc>,
    ) -> Result<ReserveOutcome, LedgerError> {
        let mut guard = UserGuard::acquire(pg, request.user_id).await?;
        guard.synchronize(self).await?;
        if limited {
            check(
                &mut guard,
                self,
                request.user_id,
                request.api_key_id,
                request.est,
                None,
            )
            .await?;
        }
        // Keep the guard through Redis admission so concurrent calls cannot both
        // spend the same remaining key budget. Refunds only make this conservative.
        self.reserve(request, now).await
    }
}
