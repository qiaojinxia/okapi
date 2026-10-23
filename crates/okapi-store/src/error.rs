/// 存储层错误。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("db_error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("db_migrate_error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    /// 等迁移门锁超过上限（持锁的进程卡死或迁移异常漫长），参数是已等的秒数。
    #[error("migration_gate_timeout: waited {0}s")]
    MigrationGateTimeout(u64),

    #[error("redis_error: {0}")]
    Redis(#[from] fred::error::Error),

    #[error("clickhouse_http_error: {0}")]
    ChHttp(#[from] reqwest::Error),

    #[error("clickhouse_status_{status}: {body}")]
    ChStatus { status: u16, body: String },

    #[error("invalid_stored_data: {0}")]
    InvalidData(&'static str),

    /// 资源被引用/状态冲突，拒绝执行（管理端需先解绑）。
    /// console 层映射为 HTTP 409 + 该 error_code，前端按语言包渲染文案。
    #[error("conflict: {0}")]
    Conflict(&'static str),
}
