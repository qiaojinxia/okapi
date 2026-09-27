use super::Error;
use super::{
    Batch, State,
    work::{self, Lease},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub enum Output<'a> {
    Success {
        content: &'a [u8],
        content_type: &'a str,
        usage: &'a Value,
    },
    Failed {
        error_code: &'a str,
        usage: &'a Value,
    },
}
#[derive(Debug, PartialEq, Eq)]
pub enum Staged {
    New,
    Replay,
}
#[derive(sqlx::FromRow)]
struct Stored {
    state: String,
    content_hash: Option<String>,
    content_type: Option<String>,
    error_code: Option<String>,
    usage: Value,
}

/// The parser must correlate a provider key with a preallocated slot. Unknown slots fail closed.
pub async fn stage(
    pg: &PgPool,
    lease: Lease,
    slot: u32,
    output: Output<'_>,
) -> Result<Staged, Error> {
    let (state, content, content_type, hash, error, usage) = match output {
        Output::Success {
            content,
            content_type,
            usage,
        } => {
            if content.is_empty()
                || content.len() > super::MAX_IMAGE_BYTES
                || !matches!(content_type, "image/png" | "image/jpeg" | "image/webp")
            {
                return Err(Error::Invalid("batch_image"));
            }
            (
                "succeeded",
                Some(content),
                Some(content_type),
                Some(hex::encode(Sha256::digest(content))),
                None,
                usage,
            )
        }
        Output::Failed { error_code, usage } => {
            if !work::error_code(error_code) {
                return Err(Error::Invalid("batch_error_code"));
            }
            ("failed", None, None, None, Some(error_code), usage)
        }
    };
    if slot >= 200 || !usage.is_object() || usage.to_string().len() > 8192 {
        return Err(Error::Invalid("batch_output"));
    }
    let (mut tx, row) = work::locked(pg, lease).await?;
    if row.state != State::Collecting {
        return Err(Error::Transition);
    }
    let slot = i32::try_from(slot).map_err(|_| Error::Invalid("batch_output"))?;
    let old:Stored=sqlx::query_as("SELECT state,content_hash,content_type,error_code,usage FROM image_batch_outputs WHERE batch_id=$1 AND slot=$2 FOR UPDATE")
        .bind(row.id).bind(slot).fetch_optional(&mut *tx).await?.ok_or(Error::UnknownOutput)?;
    if old.state != "pending" {
        if old.state != state
            || old.content_hash != hash
            || old.content_type.as_deref() != content_type
            || old.error_code.as_deref() != error
            || old.usage != *usage
        {
            return Err(Error::OutputConflict);
        }
        tx.commit().await?;
        return Ok(Staged::Replay);
    }
    sqlx::query("UPDATE image_batch_outputs SET state=$3,content=$4,content_type=$5,content_hash=$6,error_code=$7,usage=$8 WHERE batch_id=$1 AND slot=$2")
        .bind(row.id).bind(slot).bind(state).bind(content).bind(content_type).bind(hash).bind(error).bind(usage).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Staged::New)
}
/// Call only after validating all remote output files and complete EOF. Counts alone do not prove EOF.
pub async fn seal_results(pg: &PgPool, lease: Lease) -> Result<Batch, Error> {
    let (mut tx, row) = work::locked(pg, lease).await?;
    if row.state != State::Collecting {
        return Err(Error::Transition);
    }
    let (total,success,failure):(i64,i64,i64)=sqlx::query_as("SELECT COUNT(*),COUNT(*) FILTER(WHERE state='succeeded'),COUNT(*) FILTER(WHERE state='failed') FROM image_batch_outputs WHERE batch_id=$1")
        .bind(row.id).fetch_one(&mut *tx).await?;
    if total != i64::from(row.output_count) || success + failure != total {
        return Err(Error::ResultsIncomplete);
    }
    let success = i32::try_from(success).map_err(|_| Error::Invalid("batch_outputs"))?;
    let failure = i32::try_from(failure).map_err(|_| Error::Invalid("batch_outputs"))?;
    let row=sqlx::query_as("UPDATE image_batches SET state='settling',success_count=$2,failure_count=$3,results_ready_at=clock_timestamp(),updated_at=now() WHERE id=$1 RETURNING *")
        .bind(row.id).bind(success).bind(failure).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(row)
}

/// Metadata and bytes share the same owner, API key, publication and retention boundary.
pub async fn content_info_owned(
    pg: &PgPool,
    id: Uuid,
    uid: i64,
    kid: i64,
    slot: u32,
) -> Result<Option<(i32, String)>, Error> {
    let slot = i32::try_from(slot).map_err(|_| Error::Invalid("batch_output"))?;
    Ok(sqlx::query_as("SELECT octet_length(o.content),o.content_type FROM image_batch_outputs o JOIN image_batches b ON b.id=o.batch_id WHERE b.id=$1 AND b.user_id=$2 AND b.api_key_id=$3 AND NOT b.delete_requested AND b.completed_at IS NOT NULL AND b.expires_at>clock_timestamp() AND o.slot=$4 AND o.state='succeeded'").bind(id).bind(uid).bind(kid).bind(slot).fetch_optional(pg).await?)
}
/// Metadata and bytes share the same owner, API key, publication and retention boundary.
pub async fn content_owned(
    pg: &PgPool,
    id: Uuid,
    uid: i64,
    kid: i64,
    slot: u32,
) -> Result<Option<(Vec<u8>, String)>, Error> {
    let slot = i32::try_from(slot).map_err(|_| Error::Invalid("batch_output"))?;
    Ok(sqlx::query_as("SELECT o.content,o.content_type FROM image_batch_outputs o JOIN image_batches b ON b.id=o.batch_id WHERE b.id=$1 AND b.user_id=$2 AND b.api_key_id=$3 AND NOT b.delete_requested AND b.completed_at IS NOT NULL AND b.expires_at>clock_timestamp() AND o.slot=$4 AND o.state='succeeded'")
        .bind(id).bind(uid).bind(kid).bind(slot).fetch_optional(pg).await?)
}
