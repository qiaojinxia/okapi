//! 首字时间与时延的可加统计：直接读各自的预聚合物化视图。
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
    fn table(self) -> &'static str {
        match self {
            Self::Ttft => "mv_ttft_reporting_hour",
            Self::Latency => "mv_latency_reporting_hour",
        }
    }
}

/// 首字时间 / 时延读各自的预聚合（`mv_ttft_reporting_hour` / `mv_latency_reporting_hour`）。
/// 所有参数都是内部标识符与校验过的时间 / 归属谓词。
pub(super) fn source(kind: Kind, keys: &str, predicate: &str) -> String {
    let prefix = kind.prefix();
    let table = kind.table();
    let mut fields = vec![
        ("observed", "observed"),
        ("total_ms", "sum"),
        ("samples", "samples"),
    ];
    let output_aggregate = match kind {
        Kind::Ttft => "",
        Kind::Latency => {
            fields.push(("output_tokens", "output"));
            ", sumIfMerge(output_tokens) AS output_tokens"
        }
    };
    let aggregate = format!(
        "WITH toDate(hour) AS day SELECT {keys}, countMerge(requests) AS observed, sumIfMerge(total_ms) AS total_ms, countIfMerge(samples) AS samples{output_aggregate} FROM {table} WHERE {predicate}"
    );
    let source = super::measurement_coverage::aggregate_source(keys, &aggregate);
    let selected = fields
        .iter()
        .map(|(field, alias)| format!("toInt64({field}) AS {prefix}_{alias}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("(SELECT {keys}, {selected} FROM {source})")
}
