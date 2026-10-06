use super::{DIMENSIONS, Granularity, StatisticsQuery, TOOLS, schema};
use crate::StoreError;
use std::fmt::Write as _;

type Bindings = Vec<(String, String)>;

fn observation_counts(retained: bool) -> String {
    let prefix = if retained { "source" } else { "raw" };
    TOOLS
        .iter()
        .flat_map(|tool| {
            ["observed", "fee_observed"].map(|kind| {
                let count = if retained {
                    format!("toInt128(countIfMerge({tool}_{kind}))")
                } else {
                    format!("sum({tool}_{kind})")
                };
                format!("{count} AS {prefix}_{tool}_{kind}")
            })
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn richer_raw() -> String {
    let dominates = TOOLS
        .iter()
        .flat_map(|tool| {
            ["observed", "fee_observed"]
                .map(|kind| format!("raw_{tool}_{kind}>=source_{tool}_{kind}"))
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    format!(
        "(raw_n=expected_records AND source_records=expected_records AND raw_calls=expected_calls AND source_calls=expected_calls AND {dominates} AND (raw_web_search_fee_observed>source_web_search_fee_observed OR raw_web_fetch_fee_observed>source_web_fetch_fee_observed))"
    )
}

fn scope(q: &StatisticsQuery, bindings: &mut Bindings) -> Result<String, StoreError> {
    let mut predicate = "1".to_owned();
    for (name, value) in [
        ("user_id", q.filters.user_id),
        ("api_key_id", q.filters.api_key_id),
        ("channel_id", q.filters.channel_id),
    ] {
        if let Some(value) = value {
            if value < 0 {
                return Err(StoreError::InvalidData("statistics_tool_filter_invalid"));
            }
            let _ = write!(predicate, " AND {name}={{tool_{name}:Int64}}");
            bindings.push((format!("tool_{name}"), value.to_string()));
        }
    }
    for (column, name, value) in [
        (q.model_source.column(), "model", q.filters.model.as_deref()),
        ("group_code", "group", q.filters.group.as_deref()),
        ("endpoint", "endpoint", q.filters.endpoint.as_deref()),
        (
            "upstream_endpoint",
            "upstream_endpoint",
            q.filters.upstream_endpoint.as_deref(),
        ),
        ("node", "node", q.filters.node.as_deref()),
        (
            "request_type",
            "request_type",
            q.filters.request_type.as_deref(),
        ),
        (
            "billing_type",
            "billing_type",
            q.filters.billing_type.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            let _ = write!(predicate, " AND {column}={{tool_{name}:String}}");
            bindings.push((format!("tool_{name}"), value.to_owned()));
        }
    }
    if let Some(value) = q.filters.stream {
        predicate.push_str(" AND stream={tool_stream:UInt8}");
        bindings.push(("tool_stream".to_owned(), u8::from(value).to_string()));
    }
    Ok(predicate)
}

/// Select one observation source per full hour and dimension group. Missing
/// denominator rows carry no observations and are never split across dates.
pub(super) fn sql(q: &StatisticsQuery) -> Result<(String, Bindings), StoreError> {
    if q.start > q.end || (q.end - q.start).num_days() >= 366 {
        return Err(StoreError::InvalidData("statistics_tool_filter_invalid"));
    }
    let mut bindings = vec![
        ("tool_start".to_owned(), q.start.to_string()),
        ("tool_end".to_owned(), q.end.to_string()),
        (
            "tool_zone".to_owned(),
            crate::timezone::machine_timezone()?.to_owned(),
        ),
    ];
    let scope = scope(q, &mut bindings)?;
    let keys = format!("hour,{DIMENSIONS}");
    let start = "toDateTime({tool_start:Date},{tool_zone:String})";
    let end = "toDateTime({tool_end:Date}+INTERVAL 1 DAY,{tool_zone:String})";
    let coarse = format!(
        "hour>=toStartOfHour(toTimeZone({start},'UTC')) AND hour<toStartOfHour(toTimeZone({end}-INTERVAL 1 SECOND,'UTC'))+INTERVAL 1 HOUR AND {scope}"
    );
    let retained = schema::retained();
    let source_counts = observation_counts(true);
    let raw_counts = observation_counts(false);
    let richer_raw = richer_raw();
    let names = schema::measure_names();
    let measures = names
        .iter()
        .filter(|name| !matches!(name.as_str(), "covered_records" | "raw_records"))
        .cloned()
        .collect::<Vec<_>>();
    let raw = schema::observation_select();
    let raw_sum = measures
        .iter()
        .map(|name| {
            format!(
                "sum(toInt128({})) AS {name}",
                if name == "records" {
                    "financial_records"
                } else {
                    name
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let selected = measures.join(",");
    let sums = schema::sums();
    let zero = measures
        .iter()
        .filter(|name| !matches!(name.as_str(), "calls" | "records"))
        .map(|name| format!("toInt128(0) AS {name}"))
        .collect::<Vec<_>>()
        .join(",");
    let key = q.by.column(q.model_source);
    let bucket = match q.granularity {
        Granularity::Day => "toString(toDate(minute,{tool_zone:String}))",
        Granularity::Hour => "toString(toUnixTimestamp(toStartOfHour(minute)))",
    };
    // `view()` isolates reused aggregate-state column hashes in ClickHouse 24.8.
    let sql = format!(
        "WITH e AS (SELECT * FROM view(SELECT {keys},toInt128(countMerge(requests)) AS expected_calls,toInt128(countMerge(financial_records)) AS expected_records FROM mv_analysis_hour WHERE {coarse} GROUP BY {keys})), \
         a AS (SELECT * FROM view(SELECT toStartOfHour(minute) AS hour,{DIMENSIONS},toInt128(countMerge(calls)) AS source_calls,toInt128(countMerge(financial_records)) AS source_records,{source_counts} FROM server_tool_minute_v1 WHERE toStartOfHour(minute)>=toStartOfHour(toTimeZone({start},'UTC')) AND toStartOfHour(minute)<toStartOfHour(toTimeZone({end}-INTERVAL 1 SECOND,'UTC'))+INTERVAL 1 HOUR AND {scope} GROUP BY {keys})), \
         missing AS (SELECT {keys} FROM e LEFT JOIN a USING ({keys}) WHERE expected_records!=source_records OR source_web_search_fee_observed<source_records OR source_web_fetch_fee_observed<source_records), \
         raw_facts AS (SELECT minute,toStartOfHour(minute) AS hour,{DIMENSIONS},{raw_sum} FROM ({raw}) WHERE (hour,{DIMENSIONS}) IN (SELECT {keys} FROM missing) AND {scope} GROUP BY minute,{keys}), \
         r AS (SELECT {keys},sum(calls) AS raw_calls,sum(records) AS raw_n,{raw_counts} FROM raw_facts GROUP BY {keys}), \
         mode AS (SELECT {keys},expected_calls,expected_records,\
             ((source_records!=expected_records AND raw_n>source_records) OR {richer_raw}) AS use_raw,\
             if(use_raw,raw_calls,source_calls) AS covered_calls,if(use_raw,raw_n,source_records) AS covered_n,\
             throwIf(source_records>expected_records OR raw_n>expected_records OR covered_calls>expected_calls,'statistics_request_history_incomplete') AS invalid \
             FROM e LEFT JOIN a USING ({keys}) LEFT JOIN r USING ({keys})), \
         retained_facts AS (SELECT * FROM view(SELECT minute,toStartOfHour(minute) AS hour,{DIMENSIONS},{retained} FROM server_tool_minute_v1 WHERE toStartOfHour(minute)>=toStartOfHour(toTimeZone({start},'UTC')) AND toStartOfHour(minute)<toStartOfHour(toTimeZone({end}-INTERVAL 1 SECOND,'UTC'))+INTERVAL 1 HOUR AND {scope} GROUP BY minute,{keys})), \
         facts AS (SELECT minute,{DIMENSIONS},{selected},records AS covered_records,toInt128(0) AS raw_records FROM retained_facts INNER JOIN mode USING ({keys}) WHERE NOT use_raw AND invalid=0 \
             UNION ALL SELECT minute,{DIMENSIONS},{selected},records AS covered_records,records AS raw_records FROM raw_facts INNER JOIN mode USING ({keys}) WHERE use_raw AND invalid=0 \
             UNION ALL SELECT hour+toIntervalSecond(throwIf(hour<{start} OR hour+INTERVAL 1 HOUR>{end} OR toDate(hour,{{tool_zone:String}})!=toDate(hour+INTERVAL 3599 SECOND,{{tool_zone:String}}),'statistics_calendar_history_incomplete')) AS minute,{DIMENSIONS},expected_calls-covered_calls AS calls,expected_records-covered_n AS records,{zero},toInt128(0) AS covered_records,toInt128(0) AS raw_records FROM mode WHERE expected_records>covered_n AND invalid=0), \
         grouped AS (SELECT toUInt8(grouping(bucket)) AS is_total,{bucket} AS bucket,toString({key}) AS key,{sums} FROM facts WHERE minute>={start} AND minute<{end} GROUP BY GROUPING SETS ((bucket,key),())), \
         ranked AS (SELECT *,toInt128(countIf(is_total=0) OVER ()) AS total_rows,row_number() OVER (PARTITION BY is_total ORDER BY bucket,key) AS row_position FROM grouped) \
         SELECT ranked.*,guard.calendar_missing,guard.source_invalid FROM ranked CROSS JOIN \
             (SELECT countIf(expected_records>covered_n AND (hour<{start} OR hour+INTERVAL 1 HOUR>{end} OR toDate(hour,{{tool_zone:String}})!=toDate(hour+INTERVAL 3599 SECOND,{{tool_zone:String}}))) AS calendar_missing,sum(invalid) AS source_invalid FROM mode) AS guard \
             WHERE is_total=1 OR (row_position>{} AND row_position<={}) ORDER BY is_total DESC,bucket,key",
        q.offset,
        u64::from(q.offset).saturating_add(u64::from(q.limit.clamp(1, 100)))
    );
    Ok((sql, bindings))
}
