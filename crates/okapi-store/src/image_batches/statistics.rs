use super::{Batch, Error, transaction};
use chrono::{DateTime, Utc};
use okapi_domain::TokenUsage;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, sqlx::FromRow)]
pub struct Delivery {
    pub batch_id: Uuid,
    pub user_id: i64,
    pub member_user_id: Option<i64>,
    pub channel_key_id: i64,
    pub recorded_at: DateTime<Utc>,
    pub tokens: i64,
    pub amount_micro: i64,
    pub is_error: bool,
    pub lease_id: Uuid,
}

pub(super) async fn enqueue(tx: &mut Transaction<'_, Postgres>, row: &Batch) -> Result<(), Error> {
    crate::history::read_lock(tx).await?;
    let (usage, recorded_at):(serde_json::Value, DateTime<Utc>)=sqlx::query_as("SELECT h.settlement->'usage',r.created_at FROM balance_holds h JOIN billing_financial_records r ON r.request_id=h.id WHERE h.id=$1 AND h.state='closed'")
        .bind(row.id).fetch_one(&mut **tx).await?;
    let usage: TokenUsage = serde_json::from_value(usage).map_err(|_| Error::ResultsIncomplete)?;
    usage.validate().map_err(|_| Error::ResultsIncomplete)?;
    let tokens = i64::try_from(usage.total_raw()).map_err(|_| Error::ResultsIncomplete)?;
    sqlx::query("INSERT INTO image_batch_statistics(batch_id,user_id,member_user_id,channel_key_id,recorded_at,tokens,amount_micro,is_error) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(row.id).bind(row.user_id).bind(row.member_user_id).bind(row.channel_key_id)
        .bind(recorded_at).bind(tokens).bind(row.actual_micro.ok_or(Error::NotSettled)?)
        .bind(row.success_count == 0).execute(&mut **tx).await?;
    Ok(())
}

pub async fn claim(pg: &PgPool, only: Option<Uuid>) -> Result<Option<Delivery>, Error> {
    let mut tx = transaction(pg).await?;
    let id: Option<Uuid> = sqlx::query_scalar("SELECT batch_id FROM image_batch_statistics WHERE delivered_at IS NULL AND next_attempt_at<=clock_timestamp() AND (lease_until IS NULL OR lease_until<=clock_timestamp()) AND ($1::uuid IS NULL OR batch_id=$1) ORDER BY next_attempt_at,batch_id FOR UPDATE SKIP LOCKED LIMIT 1")
        .bind(only).fetch_optional(&mut *tx).await?;
    let Some(id) = id else {
        return Ok(None);
    };
    let row=sqlx::query_as("UPDATE image_batch_statistics SET lease_id=$2,lease_until=clock_timestamp()+interval '120 seconds' WHERE batch_id=$1 RETURNING *")
        .bind(id).bind(Uuid::new_v4()).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(row))
}

pub async fn acknowledge(pg: &PgPool, row: &Delivery, delivered: bool) -> Result<(), Error> {
    let mut tx = transaction(pg).await?;
    let expires:Option<DateTime<Utc>>=sqlx::query_scalar("SELECT lease_until FROM image_batch_statistics WHERE batch_id=$1 AND lease_id=$2 AND delivered_at IS NULL FOR UPDATE")
        .bind(row.batch_id).bind(row.lease_id).fetch_optional(&mut *tx).await?;
    let live: bool = sqlx::query_scalar("SELECT COALESCE($1::timestamptz>clock_timestamp(),false)")
        .bind(expires)
        .fetch_one(&mut *tx)
        .await?;
    if !live {
        return Err(Error::LeaseLost);
    }
    sqlx::query("UPDATE image_batch_statistics SET delivered_at=CASE WHEN $2 THEN clock_timestamp() ELSE NULL END,next_attempt_at=clock_timestamp()+interval '30 seconds',lease_id=NULL,lease_until=NULL WHERE batch_id=$1")
        .bind(row.batch_id).bind(delivered).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
