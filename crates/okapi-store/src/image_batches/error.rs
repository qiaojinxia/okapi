/// Execution conflicts are worker decisions, not console HTTP error codes.
/// An API adapter must explicitly map these to the public, localized error envelope.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] crate::StoreError),
    #[error("batch_invalid: {0}")]
    Invalid(&'static str),
    #[error("batch_admission_changed")]
    AdmissionChanged,
    #[error("batch_rate_limited: {0}")]
    RateLimited(&'static str),
    #[error("batch_admission_unavailable")]
    AdmissionUnavailable,
    #[error("batch_capacity")]
    Capacity,
    #[error("batch_budget")]
    Budget,
    #[error("batch_idempotency_conflict")]
    IdempotencyConflict,
    #[error("batch_lease_lost")]
    LeaseLost,
    #[error("batch_not_funded")]
    NotFunded,
    #[error("batch_not_settled")]
    NotSettled,
    #[error("batch_output_conflict")]
    OutputConflict,
    #[error("batch_parent_owner")]
    ParentOwner,
    #[error("batch_remote_identity")]
    RemoteIdentity,
    #[error("batch_remote_terminal")]
    RemoteTerminal,
    #[error("batch_results_incomplete")]
    ResultsIncomplete,
    #[error("batch_submit_intent")]
    SubmitIntent,
    #[error("batch_transition")]
    Transition,
    #[error("batch_unknown_output")]
    UnknownOutput,
}
impl From<sqlx::Error> for Error {
    fn from(value: sqlx::Error) -> Self {
        Self::Store(crate::StoreError::Sqlx(value))
    }
}
