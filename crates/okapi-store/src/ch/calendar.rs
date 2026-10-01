//! Calendar sources must have enough retained precision for the local boundary.
//! Select one complete source per hour; never add overlapping minute/hour facts.

/// Hours with complete minute coverage at exactly the requested dimensions.
fn complete_hours(dimensions: &str, coarse: &str) -> String {
    format!(
        "SELECT hour,{dimensions} FROM \
         (SELECT toStartOfHour(minute) AS hour,{dimensions},countMerge(financial_records) AS fine_n \
          FROM mv_calendar_minute GROUP BY hour,{dimensions}) AS fine \
         INNER JOIN (SELECT hour,{dimensions},countMerge(financial_records) AS coarse_n \
          FROM {coarse} GROUP BY hour,{dimensions}) AS coarse \
         USING (hour,{dimensions}) WHERE fine_n=coarse_n"
    )
}

// Fail closed when an old bucket straddles midnight. Missing history cannot be
// split proportionally or assigned to the date containing the bucket's start.
fn exact_bucket(bucket: &str, seconds: u32, zone: &str) -> String {
    format!(
        "({bucket}+toIntervalSecond(throwIf(\
         toDate({bucket},'{zone}')!=toDate({bucket}+INTERVAL {seconds} SECOND,'{zone}'),\
         'statistics_calendar_history_incomplete')))"
    )
}

fn financial_facts(zone: &str) -> String {
    const DIMENSIONS: &str = "user_id,api_key_id,group_code,model";
    let covered = complete_hours(DIMENSIONS, "mv_cube_hour");
    let minute = exact_bucket("minute", 59, zone);
    let hour = exact_bucket("hour", 3599, zone);
    let measures = "countMergeState(requests) AS requests,countMergeState(financial_records) AS financial_records,\
        sumMerge(prompt_tokens) AS prompt_tokens,sumMerge(cached_tokens) AS cached_tokens,\
        sumMerge(completion_tokens) AS completion_tokens,sumMerge(reasoning_tokens) AS reasoning_tokens,\
        sumMerge(amount) AS amount_micro,sumMerge(discount) AS discount_micro,\
        sumMerge(upstream_cost) AS upstream_cost_micro,sumMerge(errors) AS is_error";
    format!(
        "SELECT {minute} AS ts,{DIMENSIONS},{measures},sumMerge(original) AS original_amount_micro \
         FROM mv_calendar_minute AS calendar_facts \
         WHERE (toStartOfHour(minute),{DIMENSIONS}) IN ({covered}) \
         GROUP BY minute,{DIMENSIONS} \
         UNION ALL \
         SELECT {hour} AS ts,{DIMENSIONS},{measures},\
         sumMerge(amount)+sumMerge(discount) AS original_amount_micro \
         FROM mv_cube_hour WHERE (hour,{DIMENSIONS}) NOT IN ({covered}) \
         GROUP BY hour,{DIMENSIONS}"
    )
}

fn retained_days(
    dimensions: &str,
    coarse: &str,
    coverage: &str,
    fine_columns: &str,
    coarse_columns: &str,
    zone: &str,
) -> String {
    let covered = complete_hours(dimensions, coverage);
    let minute = exact_bucket("minute", 59, zone);
    let hour = exact_bucket("hour", 3599, zone);
    format!(
        "SELECT {dimensions},toDate({minute},'{zone}') AS day,{fine_columns} \
         FROM mv_calendar_minute AS calendar_facts \
         WHERE (toStartOfHour(minute),{dimensions}) IN ({covered}) \
         GROUP BY {dimensions},day \
         UNION ALL \
         SELECT {dimensions},toDate({hour},'{zone}') AS day,{coarse_columns} \
         FROM {coarse} WHERE (hour,{dimensions}) NOT IN ({covered}) \
         GROUP BY {dimensions},day"
    )
}

fn daily_select(name: &str, select: &str, zone: &str) -> String {
    let retained = retained_select(name, select, zone);
    let Some(legacy) = missing_days(name, zone) else {
        return retained;
    };
    format!("{retained} UNION ALL {legacy}")
}

struct DayHistory {
    dimensions: &'static str,
    hourly: &'static str,
    columns: &'static [(&'static str, &'static str)],
    count: &'static str,
}

const USER: &[(&str, &str)] = &[
    ("requests", "countMergeState"),
    ("financial_records", "countMergeState"),
    ("tokens", "sumMergeState"),
    ("amount", "sumMergeState"),
    ("original", "sumMergeState"),
    ("discount", "sumMergeState"),
    ("upstream_cost", "sumMergeState"),
    ("errors", "sumMergeState"),
];
const BASIC: &[(&str, &str)] = &[
    ("requests", "countMergeState"),
    ("financial_records", "countMergeState"),
    ("tokens", "sumMergeState"),
    ("amount", "sumMergeState"),
    ("discount", "sumMergeState"),
    ("errors", "sumMergeState"),
];
const MODEL: &[(&str, &str)] = &[
    ("requests", "countMergeState"),
    ("financial_records", "countMergeState"),
    ("tokens", "sumMergeState"),
    ("amount", "sumMergeState"),
    ("discount", "sumMergeState"),
];
const KEY_MODEL: &[(&str, &str)] = &[
    ("requests", "countMergeState"),
    ("financial_records", "countMergeState"),
    ("prompt_tokens", "sumMergeState"),
    ("cached_tokens", "sumMergeState"),
    ("completion_tokens", "sumMergeState"),
    ("reasoning_tokens", "sumMergeState"),
    ("amount", "sumMergeState"),
    ("discount", "sumMergeState"),
    ("errors", "sumMergeState"),
];
const CLIENT: &[(&str, &str)] = &[
    ("requests", "countMergeState"),
    ("financial_records", "countMergeState"),
    ("tokens", "sumMergeState"),
    ("amount", "sumMergeState"),
    ("errors", "sumMergeState"),
    ("users", "uniqMergeState"),
];
const WRITE: &[(&str, &str)] = &[
    ("write_tokens", "sumMergeState"),
    ("known_requests", "countIfMergeState"),
];
const REPORTED: &[(&str, &str)] = &[
    ("read_known", "countIfMergeState"),
    ("write_known", "countIfMergeState"),
    ("write_tokens", "sumMergeState"),
];

fn history(name: &str) -> Option<DayHistory> {
    let (dimensions, hourly, columns, count) = match name {
        "mv_user_day" => (
            "user_id",
            "mv_cube_hour",
            USER,
            "countMerge(v.financial_records)",
        ),
        "mv_apikey_day" => (
            "api_key_id",
            "mv_cube_hour",
            BASIC,
            "countMerge(v.financial_records)",
        ),
        "mv_group_day" => (
            "group_code",
            "mv_cube_hour",
            BASIC,
            "countMerge(v.financial_records)",
        ),
        "mv_user_model_day" => (
            "user_id,model",
            "mv_cube_hour",
            MODEL,
            "countMerge(v.financial_records)",
        ),
        "mv_key_model_day" => (
            "user_id,api_key_id,model",
            "mv_cube_hour",
            KEY_MODEL,
            "countMerge(v.financial_records)",
        ),
        "mv_client_day" => (
            "client_type",
            "mv_calendar_client_hour",
            CLIENT,
            "countMerge(v.financial_records)",
        ),
        "mv_cache_write_day" => (
            "user_id,api_key_id,model",
            "mv_calendar_cache_write_hour",
            WRITE,
            "countIfMerge(v.known_requests)",
        ),
        "mv_cache_reporting_day" => (
            "user_id,api_key_id,model",
            "mv_calendar_cache_reporting_hour",
            REPORTED,
            "countIfMerge(v.read_known)+countIfMerge(v.write_known)",
        ),
        _ => return None,
    };
    Some(DayHistory {
        dimensions,
        hourly,
        columns,
        count,
    })
}

fn missing_days(name: &str, zone: &str) -> Option<String> {
    let DayHistory {
        dimensions,
        hourly,
        columns,
        count,
    } = history(name)?;
    let merge = columns
        .iter()
        .map(|(column, function)| format!("{function}(v.{column}) AS {column}"))
        .collect::<Vec<_>>()
        .join(",");
    let checked = columns.iter().map(|(column, _)| format!("arrayElement([d.{column}],1+throwIf(d.legacy_n>ifNull(h.retained_n,0),'statistics_calendar_history_incomplete')) AS {column}")).collect::<Vec<_>>().join(",");
    let projection = dimensions
        .split(',')
        .map(|column| format!("d.{column} AS {column}"))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!(
        "SELECT {projection},arrayJoin(arrayDistinct([\
         toDate(toDateTime(d.day,'UTC'),'{zone}'),\
         toDate(toDateTime(d.day,'UTC')+INTERVAL 86399 SECOND,'{zone}')])) AS day,{checked} \
         FROM (SELECT {dimensions},day,{merge},{count} AS legacy_n FROM {name} v GROUP BY {dimensions},day) d \
         LEFT JOIN (SELECT {dimensions},toDate(hour,'UTC') AS day,{count} AS retained_n FROM {hourly} v GROUP BY {dimensions},day) h \
         USING ({dimensions},day) WHERE d.legacy_n>ifNull(h.retained_n,0)"
    ))
}

fn retained_select(name: &str, select: &str, zone: &str) -> String {
    match name {
        "mv_user_day" | "mv_apikey_day" | "mv_user_model_day" | "mv_key_model_day"
        | "mv_group_day" => format!("SELECT{select}")
            .replace(
                "countState() AS requests",
                "countMergeState(requests) AS requests,countMergeState(financial_records) AS financial_records",
            )
            .replace(
                "FROM request_log_raw",
                &format!("FROM ({})", financial_facts(zone)),
            ),
        "mv_client_day" => {
            const COLUMNS: &str = "countMergeState(requests) AS requests,countMergeState(financial_records) AS financial_records,sumMergeState(tokens) AS tokens,\
                        sumMergeState(amount) AS amount,sumMergeState(errors) AS errors";
            retained_days(
                "client_type",
                "mv_calendar_client_hour",
                "mv_calendar_client_hour",
                &format!("{COLUMNS},uniqStateIf(user_id,finalizeAggregation(calendar_facts.requests)>0) AS users"),
                &format!("{COLUMNS},uniqMergeState(users) AS users"),
                zone,
            )
        }
        "mv_cache_write_day" => retained_days(
            "user_id,api_key_id,model",
            "mv_calendar_cache_write_hour",
            "mv_cube_hour",
            "sumMergeState(write_tokens) AS write_tokens,countIfMergeState(numeric_writes) AS known_requests",
            "sumMergeState(write_tokens) AS write_tokens,countIfMergeState(known_requests) AS known_requests",
            zone,
        ),
        "mv_cache_reporting_day" => {
            const COLUMNS: &str = "countIfMergeState(read_known) AS read_known,\
                        countIfMergeState(write_known) AS write_known,sumMergeState(write_tokens) AS write_tokens";
            retained_days(
                "user_id,api_key_id,model",
                "mv_calendar_cache_reporting_hour",
                "mv_cube_hour",
                COLUMNS,
                COLUMNS,
                zone,
            )
        }
        _ => format!("SELECT{select}"),
    }
}

pub(super) fn calendar_sql(sql: &str, zone: &str) -> String {
    let mut sql = sql.to_owned();
    if zone != "UTC" && zone != "Etc/UTC" {
        let schema = include_str!("../ch_schema.sql").replace('\r', "");
        for statement in schema.split(";\n") {
            let Some(start) = statement.find("CREATE MATERIALIZED VIEW IF NOT EXISTS ") else {
                continue;
            };
            let name = statement[start + "CREATE MATERIALIZED VIEW IF NOT EXISTS ".len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default();
            if !name.ends_with("_day") || !sql.contains(name) {
                continue;
            }
            let Some((_, select)) = statement.split_once("AS SELECT") else {
                continue;
            };
            let select = daily_select(name, select, zone);
            for keyword in ["FROM", "JOIN"] {
                sql = replace_identifier(
                    &sql,
                    &format!("{keyword} {name}"),
                    &format!("{keyword} ({select}) AS {name}"),
                );
            }
        }
    }
    for column in ["ts", "hour", "ts5"] {
        let bucket = match column {
            "hour" => exact_bucket(column, 3599, zone),
            "ts5" => exact_bucket(column, 299, zone),
            _ => column.to_owned(),
        };
        sql = sql.replace(
            &format!("toDate({column})"),
            &format!("toDate({bucket}, '{zone}')"),
        );
    }
    for column in ["hour", "ts5"] {
        sql = sql.replace(
            &format!("toString({column})"),
            &format!("toString(toTimeZone({column}, '{zone}'))"),
        );
    }
    sql = sql.replace(
        "toString(toStartOfHour(hour))",
        &format!("toString(toTimeZone(toStartOfHour(hour), '{zone}'))"),
    );
    sql = sql.replace("timezone()", &format!("'{zone}'"));
    sql = sql.replace("today()", &format!("toDate(now(), '{zone}')"));
    sql.replace(
        "toStartOfDay(day)",
        &format!("toStartOfDay(toDateTime(day, '{zone}'))"),
    )
}

fn replace_identifier(sql: &str, name: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0;
    for (start, _) in sql.match_indices(name) {
        let end = start + name.len();
        let identifier = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if start > 0 && identifier(sql.as_bytes()[start - 1])
            || end < sql.len() && identifier(sql.as_bytes()[end])
        {
            continue;
        }
        out.push_str(&sql[cursor..start]);
        out.push_str(replacement);
        cursor = end;
    }
    out.push_str(&sql[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hourly_and_five_minute_dates_require_an_exact_local_bucket() {
        let sql = calendar_sql(
            "SELECT toDate(hour),toDate(ts5),toDate(ts) FROM facts",
            "Asia/Kolkata",
        );
        assert!(sql.contains("INTERVAL 3599 SECOND"));
        assert!(sql.contains("INTERVAL 299 SECOND"));
        assert!(sql.contains("statistics_calendar_history_incomplete"));
        assert!(sql.contains("toDate(ts, 'Asia/Kolkata')"));
    }

    #[test]
    fn identifiers_do_not_match_longer_table_names() {
        assert_eq!(
            replace_identifier(
                "mv_user_day_extra mv_user_day",
                "mv_user_day",
                "replacement"
            ),
            "mv_user_day_extra replacement"
        );
    }
}
