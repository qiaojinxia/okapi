//! Native batch transport. The gateway must persist ownership, pricing and submit intents
//! before using this module. A successful cancel request is not a terminal job state.
pub mod gemini;
pub mod jsonl;
mod lookup;
pub use lookup::LookupPage;
mod transport;
pub mod vertex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_INLINE_BYTES: usize = 20 * 1024 * 1024;
pub(crate) const MAX_CONTROL_BYTES: usize = 64 * 1024 * 1024;

/// Only fixed labels and numeric status are retained; upstream error bodies can contain secrets.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code} (HTTP {status:?}, outcome uncertain: {may_have_executed})")]
pub struct BatchError {
    pub code: &'static str,
    pub status: Option<u16>,
    pub retry_after_secs: Option<u64>,
    /// A submitted create/upload may have succeeded. Never automatically replay it.
    pub may_have_executed: bool,
}
impl BatchError {
    pub(crate) const fn invalid(code: &'static str) -> Self {
        Self {
            code,
            status: None,
            retry_after_secs: None,
            may_have_executed: false,
        }
    }
    pub(crate) const fn uncertain(mut self, value: bool) -> Self {
        self.may_have_executed = value;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
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
impl JobState {
    #[must_use]
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
    pub(crate) fn parse(raw: &str) -> Result<Self, BatchError> {
        let suffix = raw
            .strip_prefix("JOB_STATE_")
            .or_else(|| raw.strip_prefix("BATCH_STATE_"));
        match suffix {
            Some("PENDING" | "QUEUED") => Ok(Self::Pending),
            Some("RUNNING") => Ok(Self::Running),
            Some("CANCELLING") => Ok(Self::Cancelling),
            Some("PAUSED") => Ok(Self::Paused),
            Some("SUCCEEDED") => Ok(Self::Succeeded),
            Some("PARTIALLY_SUCCEEDED") => Ok(Self::PartiallySucceeded),
            Some("FAILED") => Ok(Self::Failed),
            Some("CANCELLED") => Ok(Self::Cancelled),
            Some("EXPIRED") => Ok(Self::Expired),
            _ => Err(BatchError::invalid("batch_unknown_state")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    File(String),
    Inline(Vec<Value>),
    GcsPrefix(String),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub name: String,
    pub state: JobState,
    pub output: Option<Output>,
    pub error_code: Option<i64>,
}

/// Provider-native generateContent JSON is kept intact, including image config and usage fields.
#[derive(Debug, Clone)]
pub struct Request {
    pub key: String,
    pub request: Value,
}

pub(crate) fn segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}
pub(crate) fn resource<'a>(value: &'a str, collection: &str) -> Result<&'a str, BatchError> {
    let (kind, id) = value
        .split_once('/')
        .ok_or_else(|| BatchError::invalid("batch_resource_name"))?;
    if kind != collection || !segment(id) {
        return Err(BatchError::invalid("batch_resource_name"));
    }
    Ok(value)
}
pub(crate) fn display_name(value: &str) -> Result<(), BatchError> {
    if value.trim().is_empty() || value.chars().count() > 512 || value.chars().any(char::is_control)
    {
        return Err(BatchError::invalid("batch_display_name"));
    }
    Ok(())
}
