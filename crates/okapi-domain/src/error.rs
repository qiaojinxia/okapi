/// 领域层错误。错误码风格（error_code），不携带自然语言给终端用户。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// token 用量字段不满足不变量（如 cached > prompt）。
    #[error("invalid_token_usage: {reason}")]
    InvalidTokenUsage { reason: &'static str },

    /// 金额算术溢出。
    #[error("money_overflow: {op}")]
    MoneyOverflow { op: &'static str },
}
