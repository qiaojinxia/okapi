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

impl StoreError {
    /// ClickHouse 这次没给出结果，是因为它本身不可用（连不上、超时、服务端 5xx、流到一半断了），
    /// 而不是查询写错了。
    ///
    /// 「读 ClickHouse，不行再读 PostgreSQL」的双后端读法只该在这种时候回退。查询错误是 ClickHouse 的 4xx
    /// （语法、类型不匹配、未知列……），回退过去只会把 bug 伪装成「暂无数据」：2026-11-22 对话存档的
    /// 同名别名就是这样，日志里只剩一条 WARN。
    #[must_use]
    pub fn is_ch_unavailable(&self) -> bool {
        match self {
            Self::ChHttp(_) => true,
            Self::ChStatus { status, body } => {
                !(400..500).contains(status) && !is_query_error(body)
            }
            _ => false,
        }
    }
}

/// 查询本身写错时 ClickHouse 报的异常码。HTTP 状态不全可靠：不少分析期错误（如 43 / 386）走缺省的 500。
const QUERY_ERROR_CODES: &[u32] = &[
    6,   // CANNOT_PARSE_TEXT
    8,   // THERE_IS_NO_COLUMN
    10,  // NOT_FOUND_COLUMN_IN_BLOCK
    16,  // NO_SUCH_COLUMN_IN_TABLE
    36,  // BAD_ARGUMENTS
    42,  // NUMBER_OF_ARGUMENTS_DOESNT_MATCH
    43,  // ILLEGAL_TYPE_OF_ARGUMENT
    44,  // ILLEGAL_COLUMN
    46,  // UNKNOWN_FUNCTION
    47,  // UNKNOWN_IDENTIFIER
    53,  // TYPE_MISMATCH
    60,  // UNKNOWN_TABLE
    62,  // SYNTAX_ERROR
    70,  // CANNOT_CONVERT_TYPE
    115, // UNKNOWN_SETTING
    184, // ILLEGAL_AGGREGATION
    215, // NOT_AN_AGGREGATE
    352, // AMBIGUOUS_COLUMN_NAME
    386, // NO_COMMON_TYPE
];

/// 体里 `Code: N.` 的 N 是不是查询错误。
fn is_query_error(body: &str) -> bool {
    body.split_once("Code: ")
        .and_then(|(_, rest)| {
            let digits = rest.split(|c: char| !c.is_ascii_digit()).next()?;
            digits.parse::<u32>().ok()
        })
        .is_some_and(|code| QUERY_ERROR_CODES.contains(&code))
}

#[cfg(test)]
mod tests {
    use super::StoreError;

    #[test]
    fn only_clickhouse_outages_count_as_unavailable() {
        let status = |status| StoreError::ChStatus {
            status,
            body: String::new(),
        };
        // 服务端出错、流到一半报错（200 头之后）：换个后端读
        assert!(status(500).is_ch_unavailable());
        assert!(status(503).is_ch_unavailable());
        assert!(status(200).is_ch_unavailable());
        // 查询本身的错：4xx，或 500 但异常码是分析期错误
        assert!(!status(400).is_ch_unavailable());
        assert!(!status(404).is_ch_unavailable());
        let shadowed = StoreError::ChStatus {
            status: 500,
            body: "Code: 43. DB::Exception: Illegal types of arguments (String, DateTime64(3)) \
                   of function greaterOrEquals. (ILLEGAL_TYPE_OF_ARGUMENT) (version 24.8.14.39 (official build))"
                .into(),
        };
        assert!(!shadowed.is_ch_unavailable());
        let busy = StoreError::ChStatus {
            status: 500,
            body:
                "Code: 241. DB::Exception: Memory limit (total) exceeded. (MEMORY_LIMIT_EXCEEDED)"
                    .into(),
        };
        assert!(busy.is_ch_unavailable());
        assert!(!StoreError::InvalidData("clickhouse_response_not_utf8").is_ch_unavailable());
        assert!(!StoreError::MigrationGateTimeout(1).is_ch_unavailable());
    }
}
