use super::Error;
use super::{Batch, State, UnitQuote, transaction};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const LEASE_SECONDS: i32 = 120;

#[derive(Clone, Copy)]
pub struct Lease {
    pub(super) id: Uuid,
    pub(super) token: Uuid,
}
pub struct Claim {
    pub batch: Batch,
    pub lease: Lease,
}
/// Private provider material; deliberately has no Debug/Serialize implementation.
#[derive(sqlx::FromRow)]
pub struct Payload {
    pub input: Vec<u8>,
    pub binding: Vec<u8>,
    pub upload_session: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteState {
    Pending,
    Running,
    Cancelling,
    Paused,
    Succeeded,
    PartiallySucceeded,
    Failed,
    Cancelled,
    Expired,
}
impl RemoteState {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Paused => "paused",
            Self::Succeeded => "succeeded",
            Self::PartiallySucceeded => "partially_succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
    pub const fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::PartiallySucceeded
                | Self::Failed
                | Self::Cancelled
                | Self::Expired
        )
    }
}
pub struct Observation<'a> {
    pub job_name: &'a str,
    pub state: RemoteState,
    pub output_ref: &'a Value,
}

pub(super) fn object(value: &Value) -> bool {
    value.is_object() && value.to_string().len() <= 65_536
}
pub(super) fn error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
pub(super) async fn locked(
    pg: &PgPool,
    lease: Lease,
) -> Result<(Transaction<'static, Postgres>, Batch), Error> {
    let mut tx = transaction(pg).await?;
    let row: Batch = sqlx::query_as("SELECT * FROM image_batches WHERE id=$1 AND lease_id=$2 AND completed_at IS NULL FOR UPDATE")
        .bind(lease.id).bind(lease.token).fetch_optional(&mut *tx).await?.ok_or(Error::LeaseLost)?;
    // WHERE predicates can be evaluated before waiting on a row lock. Recheck the
    // deadline after owning the lock, otherwise an expired executor can still submit.
    let alive: bool = sqlx::query_scalar("SELECT $1::timestamptz>clock_timestamp()")
        .bind(row.lease_until)
        .fetch_one(&mut *tx)
        .await?;
    if !alive {
        return Err(Error::LeaseLost);
    }
    Ok((tx, row))
}
async fn held(tx: &mut Transaction<'_, Postgres>, row: &Batch) -> Result<(), Error> {
    let matches: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM balance_holds WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND model_name=$4 AND request_hash=$5 AND maximum_micro=$6 AND pricing_snapshot=$7 AND state='held' AND NOT cancel_requested)")
        .bind(row.id).bind(row.user_id).bind(row.api_key_id).bind(&row.model_name).bind(&row.request_hash)
        .bind(row.maximum_micro).bind(&row.pricing_snapshot).fetch_one(&mut **tx).await?;
    if !matches {
        return Err(Error::NotFunded);
    }
    Ok(())
}

/// Reclaiming a submit intent never makes a second provider POST eligible.
pub async fn claim(pg: &PgPool, only: Option<Uuid>) -> Result<Option<Claim>, Error> {
    let mut tx = transaction(pg).await?;
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM image_batches WHERE completed_at IS NULL AND next_run_at<=clock_timestamp() AND (lease_until IS NULL OR lease_until<=clock_timestamp()) AND ($1::uuid IS NULL OR id=$1) ORDER BY next_run_at,created_at,id FOR UPDATE SKIP LOCKED LIMIT 1")
        .bind(only).fetch_optional(&mut *tx).await?;
    let Some(id) = id else {
        return Ok(None);
    };
    let lease = Lease {
        id,
        token: Uuid::new_v4(),
    };
    let batch = sqlx::query_as("UPDATE image_batches SET state=CASE WHEN state='submitting' THEN 'uncertain' ELSE state END,lease_id=$2,lease_until=clock_timestamp()+($3::int * interval '1 second'),updated_at=now() WHERE id=$1 RETURNING *")
        .bind(id).bind(lease.token).bind(LEASE_SECONDS).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(Claim { batch, lease }))
}
pub async fn payload(pg: &PgPool, lease: Lease) -> Result<Payload, Error> {
    let (mut tx, _) = locked(pg, lease).await?;
    let body = sqlx::query_as(
        "SELECT input,binding,upload_session FROM image_batch_payloads WHERE batch_id=$1",
    )
    .bind(lease.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(body)
}
pub async fn prepare(pg: &PgPool, lease: Lease) -> Result<Batch, Error> {
    let (mut tx, row) = locked(pg, lease).await?;
    if row.state != State::Funding || row.cancel_requested {
        return Err(Error::Transition);
    }
    held(&mut tx, &row).await?;
    let row = sqlx::query_as(
        "UPDATE image_batches SET state='preparing',updated_at=now() WHERE id=$1 RETURNING *",
    )
    .bind(row.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}
/// References contain resource names only. An upload session must already be sealed by the caller.
pub async fn save_input(
    pg: &PgPool,
    lease: Lease,
    input_ref: &Value,
    sealed_session: Option<&[u8]>,
) -> Result<(), Error> {
    if !object(input_ref) || sealed_session.is_some_and(|v| v.is_empty() || v.len() > 262_144) {
        return Err(Error::Invalid("batch_input_ref"));
    }
    let (mut tx, row) = locked(pg, lease).await?;
    if row.state != State::Preparing {
        return Err(Error::Transition);
    }
    sqlx::query("UPDATE image_batches SET input_ref=$2,updated_at=now() WHERE id=$1")
        .bind(row.id)
        .bind(input_ref)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE image_batch_payloads SET upload_session=$2 WHERE batch_id=$1")
        .bind(row.id)
        .bind(sealed_session)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
pub async fn mark_submitting(pg: &PgPool, lease: Lease) -> Result<Batch, Error> {
    let (mut tx, row) = locked(pg, lease).await?;
    if row.state != State::Preparing || row.cancel_requested || row.submit_intent.is_some() {
        return Err(Error::Transition);
    }
    held(&mut tx, &row).await?;
    let row=sqlx::query_as("UPDATE image_batches SET state='submitting',submit_intent=$2,updated_at=now() WHERE id=$1 RETURNING *")
        .bind(row.id).bind(Uuid::new_v4()).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(row)
}

/// A late acknowledgement records the immutable intent's remote identity even after lease loss.
/// It cannot authorize another submission, replace a job, or reopen a sealed/settled result set.
pub async fn observe(
    pg: &PgPool,
    id: Uuid,
    intent: Uuid,
    observation: Observation<'_>,
) -> Result<Batch, Error> {
    let mut tx = transaction(pg).await?;
    let row: Batch =
        sqlx::query_as("SELECT * FROM image_batches WHERE id=$1 AND submit_intent=$2 FOR UPDATE")
            .bind(id)
            .bind(intent)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(Error::SubmitIntent)?;
    apply_observation(tx, row, observation).await
}

pub(super) async fn apply_observation(
    mut tx: Transaction<'_, Postgres>,
    row: Batch,
    observation: Observation<'_>,
) -> Result<Batch, Error> {
    if observation.job_name.is_empty()
        || observation.job_name.len() > 1024
        || observation.job_name.chars().any(char::is_control)
        || !object(observation.output_ref)
    {
        return Err(Error::Invalid("batch_observation"));
    }
    if row
        .provider_job_name
        .as_deref()
        .is_some_and(|n| n != observation.job_name)
    {
        return Err(Error::RemoteIdentity);
    }
    let old_terminal = matches!(
        row.remote_state.as_deref(),
        Some("succeeded" | "partially_succeeded" | "failed" | "cancelled" | "expired")
    );
    if old_terminal {
        if observation.state.terminal()
            && (row.remote_state.as_deref() != Some(observation.state.code())
                || row.output_ref != *observation.output_ref)
        {
            return Err(Error::RemoteTerminal);
        }
        tx.commit().await?;
        return Ok(row);
    }
    if row.state.terminal() || row.state == State::Settling {
        return Err(Error::Transition);
    }
    if matches!(
        observation.state,
        RemoteState::Succeeded | RemoteState::PartiallySucceeded
    ) && observation
        .output_ref
        .as_object()
        .is_none_or(serde_json::Map::is_empty)
    {
        return Err(Error::Invalid("batch_output_ref"));
    }
    // Discard an older pending acknowledgement after observing progress.
    if observation.state == RemoteState::Pending
        && row.remote_state.as_deref().is_some_and(|s| s != "pending")
    {
        tx.commit().await?;
        return Ok(row);
    }
    let state = if observation.state.terminal() {
        "collecting"
    } else {
        "running"
    };
    let row=sqlx::query_as("UPDATE image_batches SET provider_job_name=$2,remote_state=$3,output_ref=$4,state=$5,next_run_at=now(),updated_at=now() WHERE id=$1 RETURNING *")
        .bind(row.id).bind(observation.job_name).bind(observation.state.code()).bind(observation.output_ref).bind(state).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(row)
}
pub async fn release(
    pg: &PgPool,
    lease: Lease,
    delay_seconds: u32,
    error: Option<&str>,
) -> Result<(), Error> {
    if delay_seconds > 3600 || error.is_some_and(|v| !error_code(v)) {
        return Err(Error::Invalid("batch_retry"));
    }
    let (mut tx, _) = locked(pg, lease).await?;
    sqlx::query("UPDATE image_batches SET state=CASE WHEN state='submitting' THEN 'uncertain' ELSE state END,lease_id=NULL,lease_until=NULL,next_run_at=clock_timestamp()+($2::int * interval '1 second'),error_code=$3,updated_at=now() WHERE id=$1")
        .bind(lease.id).bind(i32::try_from(delay_seconds).map_err(|_|Error::Invalid("batch_retry"))?).bind(error).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Extends only a still-live execution lease; never revives a timed-out executor.
pub async fn renew(pg: &PgPool, lease: Lease) -> Result<(), Error> {
    let (mut tx, _) = locked(pg, lease).await?;
    sqlx::query("UPDATE image_batches SET lease_until=clock_timestamp()+($2::int * interval '1 second') WHERE id=$1")
        .bind(lease.id).bind(LEASE_SECONDS).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
/// Only for a definitive provider rejection (may_have_executed=false), never a timeout.
pub async fn submission_rejected(pg: &PgPool, lease: Lease, error: &str) -> Result<Batch, Error> {
    if !error_code(error) {
        return Err(Error::Invalid("batch_error_code"));
    }
    let (mut tx, row) = locked(pg, lease).await?;
    if row.state != State::Submitting || row.provider_job_name.is_some() {
        return Err(Error::Transition);
    }
    sqlx::query("UPDATE image_batch_outputs SET state='failed',error_code=$2 WHERE batch_id=$1 AND state='pending'")
        .bind(row.id).bind(error).execute(&mut *tx).await?;
    let row=sqlx::query_as("UPDATE image_batches SET state='settling',failure_count=output_count,error_code=$2,results_ready_at=clock_timestamp(),updated_at=now() WHERE id=$1 RETURNING *")
        .bind(row.id).bind(error).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(row)
}
pub async fn abort_before_submission(
    pg: &PgPool,
    lease: Lease,
    error: &str,
) -> Result<Batch, Error> {
    if !error_code(error) {
        return Err(Error::Invalid("batch_error_code"));
    }
    let (mut tx, row) = locked(pg, lease).await?;
    if !matches!(row.state, State::Funding | State::Preparing) || row.submit_intent.is_some() {
        return Err(Error::Transition);
    }
    sqlx::query("UPDATE image_batch_outputs SET state='failed',error_code=$2 WHERE batch_id=$1 AND state='pending'")
        .bind(row.id).bind(error).execute(&mut *tx).await?;
    let row=sqlx::query_as("UPDATE image_batches SET state='settling',failure_count=output_count,error_code=$2,results_ready_at=clock_timestamp(),updated_at=now() WHERE id=$1 RETURNING *")
        .bind(row.id).bind(error).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(row)
}
/// Public completion is gated on a closed, identically owned and priced durable hold.
pub async fn finish(pg: &PgPool, lease: Lease) -> Result<Batch, Error> {
    let (mut tx, row) = locked(pg, lease).await?;
    if row.state != State::Settling || row.success_count + row.failure_count != row.output_count {
        return Err(Error::Transition);
    }
    let units = u32::try_from(row.success_count).map_err(|_| Error::Invalid("batch_outputs"))?;
    let expected = UnitQuote::read(&row.unit_quote)?.total(units)?;
    let matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM balance_holds WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND model_name=$4 AND request_hash=$5 AND maximum_micro=$6 AND pricing_snapshot=$7 AND state='closed' AND actual_micro=$8 AND settlement->'original'=$9 AND settlement->'discount'=$10 AND settlement->'list_price'=$11 AND settlement->'upstream_cost'=$12 AND settlement->'pricing'->'media_units'=$13)")
        .bind(row.id).bind(row.user_id).bind(row.api_key_id).bind(&row.model_name).bind(&row.request_hash).bind(row.maximum_micro).bind(&row.pricing_snapshot)
        .bind(expected.amount).bind(serde_json::json!(expected.original)).bind(serde_json::json!(expected.discount)).bind(serde_json::json!(expected.list_price))
        .bind(serde_json::json!(expected.upstream_cost)).bind(serde_json::json!(units)).fetch_one(&mut *tx).await?;
    if !matches {
        return Err(Error::NotSettled);
    }
    let state = if row.success_count == row.output_count {
        State::Completed
    } else if row.success_count > 0 {
        State::Partial
    } else if row.cancel_requested || row.remote_state.as_deref() == Some("cancelled") {
        State::Cancelled
    } else {
        State::Failed
    };
    let row:Batch=sqlx::query_as("UPDATE image_batches SET state=$2,actual_micro=$3,completed_at=now(),expires_at=now()+interval '7 days',lease_id=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 RETURNING *")
        .bind(row.id).bind(state.code()).bind(expected.amount).fetch_one(&mut *tx).await?;
    super::statistics::enqueue(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(row)
}
