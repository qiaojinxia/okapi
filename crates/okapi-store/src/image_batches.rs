//! Durable native batches: metadata is separate from private inputs, bindings and staged images.
mod access;
mod admission;
pub mod archive;
pub mod cleanup;
mod error;
mod listing;
mod outputs;
pub use listing::{Filters, list, list_filtered};
mod quote;
mod recovery;
pub mod statistics;
mod work;

pub use access::{Item, ItemPage, items_owned, mark_downloaded, replay, request_delete, usage};
pub use admission::{create, create_admitted};
pub use error::Error;
pub use outputs::{Output, Staged, content_info_owned, content_owned, seal_results, stage};
pub use quote::UnitQuote;
pub use recovery::{
    Recovery, adopt_recovered, conflict_recovery, recovery, recovery_page, restart_recovery,
};
pub use work::{
    Claim, Lease, Observation, Payload, RemoteState, abort_before_submission, claim, finish,
    mark_submitting, observe, payload, prepare, release, renew, save_input, submission_rejected,
};

use chrono::{DateTime, Utc};
use okapi_domain::Money;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const STORAGE_LOCK: i64 = 0x494d_4742_4154_4348;
/// Shared with the ledger: admission must serialize with pending-hold cancellation/recovery.
pub const HOLD_LOCK_NAMESPACE: i32 = 0x4248_4f4c;
pub const MAX_ACTIVE_HOLDS: i64 = 128;
pub const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const OVERHEAD_PER_OUTPUT: i64 = 16 * 1024;
const JOB_OVERHEAD: i64 = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Funding,
    Preparing,
    Submitting,
    Running,
    Collecting,
    Settling,
    Uncertain,
    Completed,
    Partial,
    Failed,
    Cancelled,
}
impl State {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Funding => "funding",
            Self::Preparing => "preparing",
            Self::Submitting => "submitting",
            Self::Running => "running",
            Self::Collecting => "collecting",
            Self::Settling => "settling",
            Self::Uncertain => "uncertain",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub const fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Partial | Self::Failed | Self::Cancelled
        )
    }
}
impl TryFrom<String> for State {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "funding" => Ok(Self::Funding),
            "preparing" => Ok(Self::Preparing),
            "submitting" => Ok(Self::Submitting),
            "running" => Ok(Self::Running),
            "collecting" => Ok(Self::Collecting),
            "settling" => Ok(Self::Settling),
            "uncertain" => Ok(Self::Uncertain),
            "completed" => Ok(Self::Completed),
            "partial" => Ok(Self::Partial),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(Error::Invalid("batch_state")),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Batch {
    pub id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub member_user_id: Option<i64>,
    pub request_hash: String,
    pub idempotency_hash: Option<String>,
    pub task_name: String,
    pub parent_id: Option<Uuid>,
    pub model_name: String,
    pub group_code: String,
    pub provider: String,
    pub channel_id: i64,
    pub channel_key_id: i64,
    pub upstream_model: String,
    pub pricing_snapshot: Value,
    pub unit_quote: Value,
    pub maximum_micro: i64,
    pub actual_micro: Option<i64>,
    pub item_count: i32,
    pub output_count: i32,
    pub success_count: i32,
    pub failure_count: i32,
    #[sqlx(try_from = "String")]
    pub state: State,
    pub cancel_requested: bool,
    pub delete_requested: bool,
    pub cleanup_done: bool,
    pub lease_id: Option<Uuid>,
    pub lease_until: Option<DateTime<Utc>>,
    pub next_run_at: DateTime<Utc>,
    pub submit_intent: Option<Uuid>,
    pub provider_job_name: Option<String>,
    pub remote_state: Option<String>,
    pub input_ref: Value,
    pub output_ref: Value,
    pub error_code: Option<String>,
    pub client_ip: Option<String>,
    pub client_type: String,
    pub storage_budget: i64,
    pub created_at: DateTime<Utc>,
    pub results_ready_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub downloaded_at: Option<DateTime<Utc>>,
}

pub struct NewItem<'a> {
    pub custom_id: &'a str,
    pub prompt_preview: &'a str,
    pub outputs: u32,
}
pub struct NewBatch<'a> {
    pub id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub request_hash: &'a str,
    pub idempotency_hash: Option<&'a str>,
    pub task_name: &'a str,
    pub parent_id: Option<Uuid>,
    pub model: &'a str,
    pub group: &'a str,
    pub provider: &'a str,
    pub channel_id: i64,
    pub channel_key_id: i64,
    pub upstream_model: &'a str,
    pub pricing: &'a Value,
    pub unit_quote: &'a Value,
    pub maximum: Money,
    pub input: &'a [u8],
    pub binding: &'a [u8],
    pub items: &'a [NewItem<'a>],
    pub client_ip: Option<&'a str>,
    pub client_type: &'a str,
}
pub enum Created {
    New(Batch),
    Existing(Batch),
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub per_user_active: i64,
    pub per_key_active: i64,
    pub per_user_jobs: i64,
    pub total_jobs: i64,
    pub per_user_bytes: i64,
    pub total_bytes: i64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            per_user_active: 8,
            per_key_active: 8,
            per_user_jobs: 1024,
            total_jobs: 16_384,
            per_user_bytes: 8 * 1024 * 1024 * 1024,
            total_bytes: 32 * 1024 * 1024 * 1024,
        }
    }
}

async fn transaction(pg: &PgPool) -> Result<Transaction<'static, Postgres>, Error> {
    let mut tx = pg.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
pub async fn owned(pg: &PgPool, id: Uuid, uid: i64, kid: i64) -> Result<Option<Batch>, Error> {
    Ok(sqlx::query_as("SELECT * FROM image_batches WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND NOT delete_requested")
        .bind(id).bind(uid).bind(kid).fetch_optional(pg).await?)
}
pub async fn cancel(pg: &PgPool, id: Uuid, uid: i64, kid: i64) -> Result<Option<Batch>, Error> {
    Ok(sqlx::query_as("UPDATE image_batches SET cancel_requested=CASE WHEN completed_at IS NULL THEN TRUE ELSE cancel_requested END,next_run_at=CASE WHEN completed_at IS NULL THEN now() ELSE next_run_at END,updated_at=now() WHERE id=$1 AND user_id=$2 AND api_key_id=$3 AND NOT delete_requested RETURNING *")
        .bind(id).bind(uid).bind(kid).fetch_optional(pg).await?)
}

pub struct Page {
    pub data: Vec<Batch>,
    pub has_more: bool,
}
