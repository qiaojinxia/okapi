//! 首字时间与时延的有效样本规则定义。
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Ttft,
    Latency,
}

impl Kind {
    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::Ttft => "ttft",
            Self::Latency => "latency",
        }
    }

    pub(super) fn valid(self) -> &'static str {
        match self {
            Self::Ttft => super::ttft_average::VALID,
            Self::Latency => "ifNull(latency_reported, toUInt8(latency_ms > 0)) = 1",
        }
    }
}
