//! 上游错误分类：对齐 IMPLEMENTATION §3.6 重试矩阵的类别。

use bytes::Bytes;

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("upstream_connect")]
    Connect(String),

    #[error("upstream_timeout")]
    Timeout,

    /// 非 2xx：body 保留用于 400 类原样转译返回；retry_after 供 429 冷却（§3.4）。
    #[error("upstream_status_{status}")]
    Status {
        status: u16,
        body: Bytes,
        retry_after_secs: Option<i64>,
    },

    /// 建流后传输错误（首字后断流不可回退）。
    #[error("upstream_stream")]
    Stream(String),

    /// 请求构造失败（body 非 JSON 等）。
    #[error("upstream_build")]
    Build(String),
}

impl UpstreamError {
    /// 首字前是否允许 failover 换渠道（§3.6）。402（上游配额/余额耗尽）同样换渠道。
    #[must_use]
    pub fn retriable_before_first_token(&self) -> bool {
        match self {
            Self::Connect(_) | Self::Timeout | Self::Stream(_) => true,
            Self::Status { status, .. } => {
                matches!(status, 401 | 402 | 403 | 408 | 429 | 500..=599)
            }
            Self::Build(_) => false,
        }
    }

    /// 是否为瞬态失败（§3.6：连接/超时/5xx 允许同 key 先重试 1 次再 failover）。
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Connect(_) | Self::Timeout | Self::Stream(_) => true,
            Self::Status { status, .. } => matches!(status, 500..=599),
            Self::Build(_) => false,
        }
    }

    /// 稳定 error_code（给客户端与账单）。
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Connect(_) | Self::Stream(_) | Self::Build(_) => "upstream_error",
            Self::Timeout => "upstream_timeout",
            Self::Status { .. } => "upstream_status",
        }
    }

    /// 上游 HTTP 状态（若有）。
    #[must_use]
    pub fn upstream_status(&self) -> Option<i16> {
        match self {
            Self::Status { status, .. } => i16::try_from(*status).ok(),
            Self::Connect(_) | Self::Timeout | Self::Stream(_) | Self::Build(_) => None,
        }
    }

    /// Retry-After（仅 429/5xx 场景可能存在）。
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<i64> {
        match self {
            Self::Status {
                retry_after_secs, ..
            } => *retry_after_secs,
            Self::Connect(_) | Self::Timeout | Self::Stream(_) | Self::Build(_) => None,
        }
    }
}
