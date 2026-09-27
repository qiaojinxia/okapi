/// 账本错误。任何账本错误都必须导致请求 fail-closed 拒绝（宁停不错账）。
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger_redis_error: {0}")]
    Redis(#[from] fred::error::Error),

    #[error("ledger_db_error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("ledger_user_not_found")]
    UserNotFound,

    #[error("ledger_unexpected_reply: {0}")]
    UnexpectedReply(&'static str),

    /// 活跃预扣已存在，不能重新准入或以新参数覆盖原记录。
    #[error("ledger_reservation_exists")]
    ReservationExists,

    #[error("ledger_invalid_reservation")]
    InvalidReservation,

    #[error("ledger_admission_state_invalid")]
    AdmissionStateInvalid,

    #[error("ledger_invalid_settlement")]
    InvalidSettlement,

    #[error("ledger_reservation_conflict")]
    ReservationConflict,

    #[error("ledger_settlement_state_invalid")]
    SettlementStateInvalid,

    #[error("ledger_hold_invalid: {0}")]
    InvalidHold(&'static str),
    #[error("ledger_hold_conflict")]
    HoldConflict,
    #[error("ledger_hold_capacity")]
    HoldCapacity,
    #[error("ledger_hold_recovery_required")]
    HoldRecoveryRequired,

    #[error("ledger_store_error: {0}")]
    Store(#[from] okapi_store::StoreError),

    /// 激活期内换别的套餐（IMPLEMENTATION §11.28；升降级 backlog）。携带当前套餐码。
    #[error("subscription_active: {0}")]
    SubscriptionActive(String),
}
