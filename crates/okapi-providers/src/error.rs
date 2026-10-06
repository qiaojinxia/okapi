//! 上游错误分类：对齐 IMPLEMENTATION §3.6 重试矩阵的类别。

use bytes::Bytes;

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("upstream_connect")]
    Connect(String),

    /// 连接阶段失败（TCP / TLS / 代理隧道 / 连接超时）：请求还没送到上游。
    /// 对外与 `Connect`（超时则与 `Timeout`）同码同语义；单独成类只为把失败归给出口代理
    /// （IMPLEMENTATION §11.41）——`Connect` 还承载凭证解析等非网络的合成原因，不能一并归因。
    /// `proxy_hop`：经代理时错误链能确定坏在代理这一跳（见 [`Self::proxy_hop_failed`]）。
    #[error("upstream_connect")]
    Unreachable {
        timed_out: bool,
        proxy_hop: bool,
        detail: String,
    },

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

    /// A persistent session may already have executed this turn. Never replay it,
    /// even if no output reached the caller before the connection failed.
    #[error("upstream_session")]
    Session {
        reason: &'static str,
        timed_out: bool,
    },

    /// 请求构造失败（body 非 JSON 等）。
    #[error("upstream_build")]
    Build(String),
}

impl UpstreamError {
    /// reqwest 的连接阶段错误 → `Unreachable`；其余返回 None 交给各家自己的分类。
    /// 连接超时同时满足 `is_connect` 与 `is_timeout`，先判连接阶段才不会被归成普通超时。
    #[must_use]
    pub fn connect_phase(e: &reqwest::Error) -> Option<Self> {
        e.is_connect().then(|| Self::Unreachable {
            timed_out: e.is_timeout(),
            proxy_hop: Self::proxy_hop_failed(e),
            detail: e.to_string(),
        })
    }

    /// 经代理的连接失败能否确定是代理这一跳的问题。按 hyper-util 的错误文本识别——它的隧道 /
    /// SOCKS 错误类型不对外公开，没法按类型判断；文本由 `proxy_hop_markers` 单测钉住。
    /// - 连不上代理：TCP / DNS 失败。隧道与 SOCKS 包成 "failed to create underlying connection"；
    ///   http 上游经 HTTP 代理不建隧道，直接是 "tcp connect error" / "dns error"。配了代理时客户端
    ///   只会连代理，这类错误不可能来自目标。
    /// - 代理拒绝认证：HTTP 407、SOCKS5 用户名密码不被接受 / 不支持。
    ///
    /// 其余（隧道或 SOCKS 握手中途断开、代理报目标不可达、目标 TLS 握手失败、超时）分不清是代理
    /// 还是目标：一个上游挂了不能把同一代理上的其他渠道一起熔断，由网关另行核实。
    #[must_use]
    pub fn proxy_hop_failed(e: &reqwest::Error) -> bool {
        const MARKERS: [&str; 8] = [
            "failed to create underlying connection",
            "tcp connect error",
            "tcp open error",
            "dns error",
            "proxy authorization required",
            "credentials not accepted",
            "server does not support user/pass authentication",
            "server implements authentication incorrectly",
        ];
        let mut source = std::error::Error::source(e);
        while let Some(err) = source {
            let text = err.to_string();
            if MARKERS.iter().any(|marker| text.contains(marker)) {
                return true;
            }
            source = err.source();
        }
        false
    }

    /// 首字前是否允许 failover 换渠道（§3.6）。402（上游配额/余额耗尽）同样换渠道。
    #[must_use]
    pub fn retriable_before_first_token(&self) -> bool {
        match self {
            Self::Connect(_) | Self::Unreachable { .. } | Self::Timeout | Self::Stream(_) => true,
            Self::Status { status, .. } => {
                matches!(status, 401 | 402 | 403 | 408 | 429 | 500..=599)
            }
            Self::Build(reason) => reason.starts_with("channel_control:"),
            Self::Session { .. } => false,
        }
    }

    /// 是否为瞬态失败（§3.6：连接/超时/5xx 允许同 key 先重试 1 次再 failover）。
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Connect(_) | Self::Unreachable { .. } | Self::Timeout | Self::Stream(_) => true,
            Self::Status { status, .. } => matches!(status, 500..=528 | 530..=599),
            Self::Build(_) | Self::Session { .. } => false,
        }
    }

    /// 稳定 error_code（给客户端与账单）。
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Build(reason) if reason.starts_with("channel_control:") => "no_available_channel",
            Self::Timeout
            | Self::Unreachable {
                timed_out: true, ..
            }
            | Self::Session {
                timed_out: true, ..
            } => "upstream_timeout",
            Self::Connect(_)
            | Self::Unreachable { .. }
            | Self::Stream(_)
            | Self::Build(_)
            | Self::Session { .. } => "upstream_error",
            Self::Status { .. } => "upstream_status",
        }
    }

    /// 上游 HTTP 状态（若有）。
    #[must_use]
    pub fn upstream_status(&self) -> Option<i16> {
        match self {
            Self::Status { status, .. } => i16::try_from(*status).ok(),
            Self::Connect(_)
            | Self::Unreachable { .. }
            | Self::Timeout
            | Self::Stream(_)
            | Self::Build(_)
            | Self::Session { .. } => None,
        }
    }

    /// Retry-After（仅 429/5xx 场景可能存在）。
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<i64> {
        match self {
            Self::Status {
                retry_after_secs, ..
            } => *retry_after_secs,
            Self::Connect(_)
            | Self::Unreachable { .. }
            | Self::Timeout
            | Self::Stream(_)
            | Self::Build(_)
            | Self::Session { .. } => None,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
