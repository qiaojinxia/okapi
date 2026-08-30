use okapi_domain::DomainError;

/// 请求路径（engine）错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PricingError {
    #[error("unknown_model: {0}")]
    UnknownModel(String),

    #[error("unknown_group: {0}")]
    UnknownGroup(String),

    /// 整数溢出（i128 中间量或落回 i64 失败）。fail-closed：调用方必须拒绝请求。
    #[error("pricing_overflow")]
    Overflow,

    /// 内部不变量被破坏（编译期校验应已排除；fail-closed 防御）。
    #[error("pricing_internal: {0}")]
    Internal(&'static str),

    #[error(transparent)]
    InvalidUsage(#[from] DomainError),
}

/// 配置编译（PriceBook）错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    #[error("duplicate_model: {0}")]
    DuplicateModel(String),

    #[error("duplicate_group: {0}")]
    DuplicateGroup(String),

    #[error("duplicate_override: user={user} model={model}")]
    DuplicateOverride { user: i64, model: String },

    #[error("invalid_tier_table: {model}: {reason}")]
    InvalidTierTable { model: String, reason: &'static str },

    #[error("invalid_absolute_override: user={user} model={model}: {reason}")]
    InvalidAbsoluteOverride {
        user: i64,
        model: String,
        reason: &'static str,
    },
}
