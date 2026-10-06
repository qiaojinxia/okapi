//! Persistent long-lived holds. PG owns identity/state; Redis owns admission.
//! No provider call may start until `reserve` confirms `Held`.
mod db;
mod hot;
mod repair;
mod settle;

use crate::{BalanceLedger, LedgerError, Pool};
use chrono::{DateTime, Utc};
use okapi_domain::Money;
use okapi_pricing::PricingSnapshot;
use serde_json::Value;
use sqlx::{PgPool, Postgres, pool::PoolConnection};
use uuid::Uuid;

pub use repair::Repaired;
pub use settle::settle;
pub const MAXIMUM_MICROS: i64 = 9_007_199_254_740_991;
const MAX_ACTIVE: i64 = okapi_store::image_batches::MAX_ACTIVE_HOLDS;
const LOCK_NAMESPACE: i32 = okapi_store::image_batches::HOLD_LOCK_NAMESPACE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Held,
    Closing,
    Closed,
}
impl TryFrom<String> for Status {
    type Error = LedgerError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        match raw.as_str() {
            "pending" => Ok(Self::Pending),
            "held" => Ok(Self::Held),
            "closing" => Ok(Self::Closing),
            "closed" => Ok(Self::Closed),
            _ => Err(LedgerError::InvalidHold("state")),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Hold {
    pub id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub model_name: String,
    pub request_hash: String,
    pub maximum_micro: i64,
    pub pricing_snapshot: Value,
    #[sqlx(rename = "state", try_from = "String")]
    pub status: Status,
    pub pool: Option<i16>,
    pub source_window: Option<String>,
    pub actual_micro: Option<i64>,
    pub credit_micro: Option<i64>,
    pub settlement: Option<Value>,
    pub cancel_requested: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl Hold {
    fn pool(&self) -> Result<Pool, LedgerError> {
        match self.pool {
            Some(0) => Ok(Pool::Wallet),
            Some(1) => Ok(Pool::Subscription),
            _ => Err(LedgerError::InvalidHold("pool")),
        }
    }
}
pub struct Reserve<'a> {
    pub id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub model: &'a str,
    pub request_hash: &'a str,
    pub maximum: Money,
    pub pricing: &'a PricingSnapshot,
}
/// An immutable snapshot loaded from the service's durable job record, never from a client body.
pub struct FrozenReserve<'a> {
    pub id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub model: &'a str,
    pub request_hash: &'a str,
    pub maximum: Money,
    pub pricing: &'a Value,
}
#[derive(Debug)]
pub enum Admission {
    Held { hold: Hold, replayed: bool },
    Closed(Hold),
    Insufficient { balance: Money },
    ConcurrencyLimited,
}

/// A dedicated session lock survives PG commits between a durable intent and Redis IO.
/// close_on_drop prevents a cancelled task from returning a locked connection to the pool.
pub struct UserGuard {
    connection: Option<PoolConnection<Postgres>>,
    user_id: i64,
}
impl Drop for UserGuard {
    fn drop(&mut self) {
        let Some(mut connection) = self.connection.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let user_id = self.user_id.to_string();
        runtime.spawn(async move {
            // close_on_drop remains armed on every error, cancellation or shutdown.
            // Reuse is explicit and only follows a verified unlock and session reset.
            let cleanup = async {
                let unlocked: bool =
                    sqlx::query_scalar("SELECT pg_advisory_unlock($1, hashtext($2))")
                        .bind(LOCK_NAMESPACE)
                        .bind(user_id)
                        .fetch_one(&mut *connection)
                        .await?;
                if !unlocked {
                    return Err(sqlx::Error::Protocol("user lock not held".into()));
                }
                sqlx::query("RESET lock_timeout")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("RESET statement_timeout")
                    .execute(&mut *connection)
                    .await?;
                Ok::<(), sqlx::Error>(())
            };
            if matches!(
                tokio::time::timeout(std::time::Duration::from_secs(3), cleanup).await,
                Ok(Ok(()))
            ) {
                // sqlx 0.9's explicit return transfers the live connection even with
                // close_on_drop armed; the now-empty wrapper cannot close it twice.
                connection.return_to_pool().await;
            }
        });
    }
}

impl UserGuard {
    /// Use the locked connection for PG work, including when the pool has only one slot.
    pub fn connection(&mut self) -> Result<&mut sqlx::PgConnection, LedgerError> {
        self.connection
            .as_deref_mut()
            .ok_or(LedgerError::HoldRecoveryRequired)
    }
    pub async fn acquire(pg: &PgPool, user_id: i64) -> Result<Self, LedgerError> {
        let mut connection = pg.acquire().await?;
        connection.close_on_drop();
        sqlx::query("SET lock_timeout = '5s'")
            .execute(&mut *connection)
            .await?;
        sqlx::query("SET statement_timeout = '10s'")
            .execute(&mut *connection)
            .await?;
        sqlx::query("SELECT pg_advisory_lock($1, hashtext($2))")
            .bind(LOCK_NAMESPACE)
            .bind(user_id.to_string())
            .execute(&mut *connection)
            .await?;
        Ok(Self {
            connection: Some(connection),
            user_id,
        })
    }
    pub(crate) async fn subscription_frozen(
        &mut self,
        ledger: &BalanceLedger,
    ) -> Result<Money, LedgerError> {
        let mut total = Money::ZERO;
        for reservation in ledger.list_reservations(self.user_id).await? {
            if reservation.pool == crate::Pool::Subscription && reservation.source_window.is_none()
            {
                // An old receipt cannot prove its period. Let it close before
                // accepting a new period instead of inventing ownership.
                return Err(LedgerError::HoldRecoveryRequired);
            }
        }
        // Ordinary reservations belong to the period being closed. Their
        // unspent quota expires now; a late actual charge revises that expiry
        // in sync::record. Durable holds keep their separately persisted funds.
        let mut active = hot::active(ledger, self.user_id).await?;
        let rows: Vec<Hold> = sqlx::query_as(
            "SELECT * FROM balance_holds WHERE user_id=$1 AND state IN ('pending','held')",
        )
        .bind(self.user_id)
        .fetch_all(self.connection()?)
        .await?;
        for row in rows {
            let hot = active.remove(&row.id);
            if let Some(hot) = &hot
                && (hot.amount != row.maximum_micro.to_string()
                    || hot.key != row.api_key_id.to_string()
                    || hot.proof != row.request_hash
                    || (row.status == Status::Held
                        && crate::Pool::from_i16(hot.pool) != row.pool()?))
            {
                return Err(LedgerError::HoldConflict);
            }
            let pool = match row.status {
                Status::Held => Some(row.pool()?),
                Status::Pending => hot.as_ref().map(|h| crate::Pool::from_i16(h.pool)),
                _ => return Err(LedgerError::HoldConflict),
            };
            if pool == Some(crate::Pool::Subscription) {
                total = total
                    .checked_add(Money::from_micros(row.maximum_micro))
                    .ok_or(LedgerError::InvalidHold("subscription_amount"))?;
            }
        }
        if !active.is_empty() {
            return Err(LedgerError::InvalidHold("unknown_hold"));
        }
        Ok(total)
    }
    /// Before changing a subscription window, finish every already-committed settlement.
    pub async fn synchronize(&mut self, ledger: &BalanceLedger) -> Result<(), LedgerError> {
        // Every admission and settlement runs this inside the user's lock. One probe
        // covers the four recovery queues, so an idle fence costs a single round trip.
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM billing_sync WHERE user_id = $1)
                 OR EXISTS(SELECT 1 FROM balance_holds WHERE user_id = $1 AND state = 'closing')
                 OR EXISTS(SELECT 1 FROM fund_transfers WHERE user_id = $1 AND cleaned_at IS NULL)
                 OR EXISTS(SELECT 1 FROM subscription_sync WHERE user_id = $1)",
        )
        .bind(self.user_id)
        .fetch_one(self.connection()?)
        .await?;
        if !pending {
            return Ok(());
        }
        crate::sync::synchronize(self, ledger, self.user_id).await?;
        let rows = db::closing(self).await?;
        for row in rows {
            hot::close(ledger, &row).await?;
            db::closed(self, row.id).await?;
        }
        crate::transfers::synchronize(self, ledger, self.user_id).await?;
        crate::subscriptions::effects::synchronize(self, ledger, self.user_id).await?;
        Ok(())
    }
}

/// Long-lived holds are deliberately excluded from synchronous reservation expiry.
pub async fn inflight(ledger: &BalanceLedger, user_id: i64) -> Result<(Money, Money), LedgerError> {
    let mut wallet = Money::ZERO;
    let mut sub = Money::ZERO;
    for hold in hot::active(ledger, user_id).await?.into_values() {
        let target = match hold.pool {
            0 => &mut wallet,
            1 => &mut sub,
            _ => return Err(LedgerError::InvalidHold("pool")),
        };
        *target = target
            .checked_add(hold.amount()?)
            .ok_or(LedgerError::InvalidHold("sum"))?;
    }
    Ok((wallet, sub))
}

pub async fn reserve(
    pg: &PgPool,
    ledger: &BalanceLedger,
    request: Reserve<'_>,
    now: DateTime<Utc>,
) -> Result<Admission, LedgerError> {
    let pricing =
        serde_json::to_value(request.pricing).map_err(|_| LedgerError::InvalidHold("pricing"))?;
    reserve_frozen(
        pg,
        ledger,
        FrozenReserve {
            id: request.id,
            user_id: request.user_id,
            api_key_id: request.api_key_id,
            model: request.model,
            request_hash: request.request_hash,
            maximum: request.maximum,
            pricing: &pricing,
        },
        now,
    )
    .await
}

/// Resume admission after a worker restart using the originally persisted price, not today's quote.
pub async fn reserve_frozen(
    pg: &PgPool,
    ledger: &BalanceLedger,
    request: FrozenReserve<'_>,
    now: DateTime<Utc>,
) -> Result<Admission, LedgerError> {
    let mut guard = UserGuard::acquire(pg, request.user_id).await?;
    guard.synchronize(ledger).await?;
    let existing: Option<String> =
        sqlx::query_scalar("SELECT state FROM balance_holds WHERE id=$1")
            .bind(request.id)
            .fetch_optional(guard.connection()?)
            .await?;
    if existing.as_deref().is_none_or(|state| state == "pending") {
        crate::key_budget::check(
            &mut guard,
            ledger,
            request.user_id,
            request.api_key_id,
            request.maximum,
            Some(request.id),
        )
        .await?;
    }
    let (mut hold, created) = db::intent(&mut guard, &request).await?;
    if hold.cancel_requested && hold.status != Status::Closed {
        return Err(LedgerError::HoldRecoveryRequired);
    }
    match hold.status {
        Status::Closed => return Ok(Admission::Closed(hold)),
        Status::Closing => return Err(LedgerError::HoldRecoveryRequired),
        Status::Held => {
            hot::verify(ledger, &hold).await?;
            return Ok(Admission::Held {
                hold,
                replayed: true,
            });
        }
        Status::Pending => {}
    }
    let window = db::window(&mut guard, now).await?;
    let concurrency = db::concurrency(&mut guard, hold.api_key_id).await?;
    match hot::reserve(ledger, &hold, now, window, concurrency).await? {
        hot::Reserved::ConcurrencyLimited => Ok(Admission::ConcurrencyLimited),
        hot::Reserved::Insufficient(balance) => Ok(Admission::Insufficient { balance }),
        hot::Reserved::Held(receipt) => {
            hold = db::held(&mut guard, hold.id, &receipt).await?;
            Ok(Admission::Held {
                hold,
                replayed: !created,
            })
        }
    }
}
