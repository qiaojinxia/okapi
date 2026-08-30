/// 账本错误。任何账本错误都必须导致请求 fail-closed 拒绝（宁停不错账）。
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger_redis_error: {0}")]
    Redis(#[from] fred::error::Error),

    #[error("ledger_db_error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("ledger_unexpected_reply: {0}")]
    UnexpectedReply(&'static str),
}
