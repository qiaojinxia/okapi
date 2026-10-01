//! Coverage-aware additive measurements. Raw and MV candidates overlap, never add them.
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

/// All arguments are internal identifiers and validated time/owner predicates.
pub(super) fn source(kind: Kind, keys: &str, expected_table: &str, predicate: &str) -> String {
    prepared(
        kind,
        keys,
        expected_table,
        predicate,
        super::measurement_coverage::Mode::Recover,
    )
}

pub(super) fn prepared(
    kind: Kind,
    keys: &str,
    expected_table: &str,
    predicate: &str,
    mode: super::measurement_coverage::Mode,
) -> String {
    let prefix = kind.prefix();
    let valid = kind.valid();
    let table = kind.table();
    let selected_keys = keys
        .split(", ")
        .map(|key| format!("e.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut fields = vec![
        ("observed", "observed"),
        ("total_ms", "sum"),
        ("samples", "samples"),
    ];
    let (output_aggregate, output_raw) = match kind {
        Kind::Ttft => (String::new(), String::new()),
        Kind::Latency => {
            fields.push(("output_tokens", "output"));
            (
                ", sumIfMerge(output_tokens) AS output_tokens".to_owned(),
                format!(", sumIf(toUInt64(completion_tokens), {valid}) AS output_tokens"),
            )
        }
    };
    if mode != super::measurement_coverage::Mode::Recover {
        let expected = format!(
            "WITH toDate(hour) AS day SELECT {keys}, countMerge(requests) AS n FROM {expected_table} WHERE {predicate} GROUP BY {keys}"
        );
        let counts = format!(
            "WITH toDate(hour) AS day SELECT {keys}, countMerge(requests) AS n FROM {table} WHERE {predicate} GROUP BY {keys}"
        );
        let aggregate = format!(
            "WITH toDate(hour) AS day SELECT {keys}, countMerge(requests) AS observed, sumIfMerge(total_ms) AS total_ms, countIfMerge(samples) AS samples{output_aggregate} FROM {table} WHERE {predicate}"
        );
        let raw = format!(
            "WITH toStartOfHour(ts) AS hour, toDate(ts) AS day SELECT {keys}, count() AS observed, sumIf(toUInt64({prefix}_ms), {valid}) AS total_ms, countIf({valid}) AS samples{output_raw} FROM request_log_calls WHERE {predicate}"
        );
        let source = super::measurement_coverage::fast_source(
            mode, keys, &expected, &counts, &aggregate, &raw,
        );
        let selected = fields
            .iter()
            .map(|(field, alias)| format!("toInt64({field}) AS {prefix}_{alias}"))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("(SELECT {keys}, {selected} FROM {source})");
    }
    let selected = fields.iter().map(|(field, alias)| {
        format!("toInt64(if(use_aggregate, ifNull(a.{field}, 0), if(ifNull(r.observed, 0) <= e.expected, ifNull(r.{field}, 0), 0))) AS {prefix}_{alias}")
    }).collect::<Vec<_>>().join(", ");
    format!(
        "(WITH \
        e AS (SELECT {keys}, countMerge(requests) AS expected FROM (SELECT *, toDate(hour) AS day FROM {expected_table} WHERE {predicate}) WHERE {predicate} GROUP BY {keys}), \
        a AS (SELECT {keys}, countMerge(requests) AS observed, sumIfMerge(total_ms) AS total_ms, countIfMerge(samples) AS samples{output_aggregate} FROM (SELECT *, toDate(hour) AS day FROM {table}) WHERE {predicate} GROUP BY {keys}), \
        missing AS (SELECT {keys} FROM e LEFT JOIN a USING ({keys}) WHERE e.expected != ifNull(a.observed, 0)), \
        r AS (SELECT {keys}, count() AS observed, sumIf(toUInt64({prefix}_ms), {valid}) AS total_ms, countIf({valid}) AS samples{output_raw} \
            FROM (SELECT *, toStartOfHour(ts) AS hour, toDate(ts) AS day FROM request_log_calls) raw_rows INNER JOIN missing USING ({keys}) WHERE {predicate} GROUP BY {keys}) \
        SELECT {selected_keys}, {selected}, \
            (ifNull(a.observed, 0) = e.expected OR (ifNull(a.observed, 0) < e.expected AND (ifNull(r.observed, 0) > e.expected OR ifNull(a.observed, 0) >= ifNull(r.observed, 0)))) AS use_aggregate \
        FROM e LEFT JOIN a USING ({keys}) LEFT JOIN r USING ({keys}))"
    )
}
