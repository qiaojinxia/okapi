//! Durable credits/refunds. PG owns acceptance; Redis owns available balance.
//! A receipt in the same hash makes an ambiguous Redis acknowledgement replayable.
use crate::{BalanceLedger, LedgerError, Pool, holds::UserGuard};
use fred::interfaces::{HashesInterface, LuaInterface};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug)]
pub struct Receipt {
    pub operation_id: Uuid,
    /// None means durable acceptance with hot-ledger application still pending.
    pub balance_after: Option<Money>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct Transfer {
    pub id: Uuid,
    pub sequence: i64,
    pub amount_micro: i64,
    pub pool: i16,
    pub applied_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl Transfer {
    pub(crate) fn manifest(&self) -> Value {
        json!({"id": self.id, "receipt": format!("{}|{}", Pool::from_i16(self.pool).field(), self.amount_micro)})
    }
}

/// Caller holds UserGuard and commits the associated business transition/event
/// in this transaction. No Redis IO is allowed before that commit.
pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    amount: Money,
    pool: Pool,
) -> Result<Uuid, LedgerError> {
    if !(-crate::holds::MAXIMUM_MICROS..=crate::holds::MAXIMUM_MICROS).contains(&amount.as_micros())
    {
        return Err(LedgerError::InvalidSettlement);
    }
    let id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO fund_transfers(id,user_id,amount_micro,pool) VALUES ($1,$2,$3,$4)",
        id,
        user_id,
        amount.as_micros(),
        pool.as_i16()
    )
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

pub async fn credit_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    amount: Money,
    event_type: &str,
    actor: &str,
    payload: Value,
) -> Result<Uuid, LedgerError> {
    crate::pg::record_credit_in_tx(tx, user_id, amount, event_type, actor, payload).await?;
    let balance = sqlx::query_scalar!("SELECT balance_micro FROM users WHERE id=$1", user_id)
        .fetch_one(&mut **tx)
        .await?;
    if !(-crate::holds::MAXIMUM_MICROS..=crate::holds::MAXIMUM_MICROS).contains(&balance) {
        return Err(LedgerError::InvalidSettlement);
    }
    enqueue(tx, user_id, amount, Pool::Wallet).await
}

/// Once PG accepts an operation, report pending rather than inviting the caller
/// to submit a second credit on a transient hot-ledger failure.
pub async fn finish(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    user_id: i64,
    operation_id: Uuid,
    pool: Pool,
) -> Receipt {
    let balance_after = match guard.synchronize(ledger).await {
        Ok(()) => match pool {
            Pool::Wallet => ledger.balance(user_id).await.ok(),
            Pool::Subscription => ledger
                .sub_balance(user_id)
                .await
                .ok()
                .map(|(balance, _)| balance),
        },
        Err(error) => {
            tracing::error!(user_id,%operation_id,%error,"durable funds awaiting recovery");
            None
        }
    };
    Receipt {
        operation_id,
        balance_after,
    }
}

pub(crate) async fn pending(
    guard: &mut UserGuard,
    user_id: i64,
) -> Result<Vec<Transfer>, LedgerError> {
    Ok(sqlx::query_as!(Transfer,"SELECT id,sequence,amount_micro,pool,applied_at FROM fund_transfers WHERE user_id=$1 AND cleaned_at IS NULL ORDER BY sequence",user_id)
        .fetch_all(guard.connection()).await?)
}

/// Includes completed operations: repair must also reject their delayed commands.
pub(crate) async fn high_water(guard: &mut UserGuard, user_id: i64) -> Result<i64, LedgerError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COALESCE(MAX(sequence),0) AS "sequence!" FROM fund_transfers WHERE user_id=$1"#,
        user_id
    )
    .fetch_one(guard.connection())
    .await?)
}

pub(crate) async fn synchronize(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    user_id: i64,
) -> Result<(), LedgerError> {
    let rows = pending(guard, user_id).await?;
    for row in rows {
        if row.applied_at.is_none() {
            let result: String = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                ledger.client().eval(
                    include_str!("lua/fund_transfer.lua"),
                    vec![format!("bal:{{{user_id}}}")],
                    vec![
                        row.id.to_string(),
                        row.amount_micro.to_string(),
                        Pool::from_i16(row.pool).field().to_owned(),
                        row.sequence.to_string(),
                    ],
                ),
            )
            .await
            .map_err(|_| LedgerError::HoldRecoveryRequired)??;
            match result.as_str() {
                "applied" => {}
                "missing" => {
                    // Whole-key loss also loses its receipts. Rebuild from PG and
                    // advance the durable sequence atomically with the balances.
                    let totals = okapi_store::history::totals(guard.connection(), user_id).await?;
                    let wallet =
                        sqlx::query_scalar!("SELECT balance_micro FROM users WHERE id=$1", user_id)
                            .fetch_one(guard.connection())
                            .await?;
                    guard
                        .repair(
                            ledger,
                            Money::from_micros(wallet),
                            Money::from_micros(totals.subscription),
                        )
                        .await?;
                }
                _ => return Err(LedgerError::SettlementStateInvalid),
            }
            sqlx::query!(
                "UPDATE fund_transfers SET applied_at=now() WHERE id=$1 AND applied_at IS NULL",
                row.id
            )
            .execute(guard.connection())
            .await?;
        }
        // PG first. If cleanup or its ACK fails, retry HDEL without adding money.
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            ledger
                .client()
                .hdel::<(), _, _>(format!("bal:{{{user_id}}}"), format!("c:{}", row.id)),
        )
        .await
        .map_err(|_| LedgerError::HoldRecoveryRequired)??;
        sqlx::query!(
            "UPDATE fund_transfers SET cleaned_at=now() WHERE id=$1",
            row.id
        )
        .execute(guard.connection())
        .await?;
    }
    Ok(())
}

pub async fn recover_pending(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
) -> Result<usize, LedgerError> {
    let users = sqlx::query_scalar!("SELECT user_id FROM fund_transfers WHERE cleaned_at IS NULL GROUP BY user_id ORDER BY min(created_at),user_id LIMIT $1",limit.clamp(1,1000))
        .fetch_all(pg).await?;
    let mut recovered = 0;
    for user_id in users {
        let result = async {
            let mut guard = UserGuard::acquire(pg, user_id).await?;
            guard.synchronize(ledger).await
        }
        .await;
        match result {
            Ok(()) => recovered += 1,
            Err(error) => tracing::error!(user_id,%error,"fund recovery deferred"),
        }
    }
    Ok(recovered)
}
