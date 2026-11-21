//! Calendar sources resolve daily views directly from minute-precision facts
//! (`mv_calendar_minute`) so every local timezone boundary (including fractional-hour
//! offsets and DST transitions) is exact without maintaining separate daily/hourly MVs.

mod lexical;
mod scope;
use std::fmt::Write as _;

const DAILY_VIEWS: &[&str] = &[
    "mv_user_day",
    "mv_apikey_day",
    "mv_group_day",
    "mv_user_model_day",
    "mv_key_model_day",
    "mv_client_day",
    "mv_cache_write_day",
    "mv_cache_reporting_day",
];

// Fail closed when an old bucket straddles midnight. Missing history cannot be
// split proportionally or assigned to the date containing the bucket's start.
fn exact_bucket(bucket: &str, seconds: u32, zone: &str) -> String {
    format!(
        "({bucket}+toIntervalSecond(throwIf(\
         toDate({bucket},'{zone}')!=toDate({bucket}+INTERVAL {seconds} SECOND,'{zone}'),\
         'statistics_calendar_history_incomplete')))"
    )
}

fn daily_select(name: &str, zone: &str) -> Option<String> {
    let day = format!("toDate(minute,'{zone}') AS day");
    let sql = match name {
        "mv_user_day" => format!(
            "SELECT user_id,{day},\
             countMergeState(requests) AS requests,\
             countMergeState(financial_records) AS financial_records,\
             sumMergeState(tokens) AS tokens,\
             sumMergeState(amount) AS amount,\
             sumMergeState(original) AS original,\
             sumMergeState(discount) AS discount,\
             sumMergeState(upstream_cost) AS upstream_cost,\
             sumMergeState(errors) AS errors \
             FROM mv_calendar_minute GROUP BY user_id,day"
        ),
        "mv_apikey_day" => format!(
            "SELECT api_key_id,{day},\
             countMergeState(requests) AS requests,\
             countMergeState(financial_records) AS financial_records,\
             sumMergeState(tokens) AS tokens,\
             sumMergeState(amount) AS amount,\
             sumMergeState(discount) AS discount,\
             sumMergeState(errors) AS errors \
             FROM mv_calendar_minute GROUP BY api_key_id,day"
        ),
        "mv_group_day" => format!(
            "SELECT group_code,{day},\
             countMergeState(requests) AS requests,\
             countMergeState(financial_records) AS financial_records,\
             sumMergeState(tokens) AS tokens,\
             sumMergeState(amount) AS amount,\
             sumMergeState(discount) AS discount,\
             sumMergeState(errors) AS errors \
             FROM mv_calendar_minute GROUP BY group_code,day"
        ),
        "mv_user_model_day" => format!(
            "SELECT user_id,model,{day},\
             countMergeState(requests) AS requests,\
             countMergeState(financial_records) AS financial_records,\
             sumMergeState(tokens) AS tokens,\
             sumMergeState(amount) AS amount,\
             sumMergeState(discount) AS discount \
             FROM mv_calendar_minute GROUP BY user_id,model,day"
        ),
        "mv_key_model_day" => format!(
            "SELECT user_id,api_key_id,model,{day},\
             countMergeState(requests) AS requests,\
             countMergeState(financial_records) AS financial_records,\
             sumMergeState(prompt_tokens) AS prompt_tokens,\
             sumMergeState(cached_tokens) AS cached_tokens,\
             sumMergeState(completion_tokens) AS completion_tokens,\
             sumMergeState(reasoning_tokens) AS reasoning_tokens,\
             sumMergeState(amount) AS amount,\
             sumMergeState(discount) AS discount,\
             sumMergeState(errors) AS errors \
             FROM mv_calendar_minute GROUP BY user_id,api_key_id,model,day"
        ),
        "mv_client_day" => format!(
            "SELECT client_type,{day},\
             countMergeState(requests) AS requests,\
             countMergeState(financial_records) AS financial_records,\
             sumMergeState(tokens) AS tokens,\
             sumMergeState(amount) AS amount,\
             sumMergeState(errors) AS errors,\
             uniqStateIf(user_id,finalizeAggregation(calendar_facts.requests)>0) AS users \
             FROM mv_calendar_minute AS calendar_facts GROUP BY client_type,day"
        ),
        "mv_cache_write_day" => format!(
            "SELECT user_id,api_key_id,model,{day},\
             sumMergeState(write_tokens) AS write_tokens,\
             countIfMergeState(numeric_writes) AS known_requests \
             FROM mv_calendar_minute GROUP BY user_id,api_key_id,model,day"
        ),
        "mv_cache_reporting_day" => format!(
            "SELECT user_id,api_key_id,model,{day},\
             countIfMergeState(read_known) AS read_known,\
             countIfMergeState(write_known) AS write_known,\
             sumMergeState(write_tokens) AS write_tokens \
             FROM mv_calendar_minute GROUP BY user_id,api_key_id,model,day"
        ),
        _ => return None,
    };
    Some(sql)
}

pub(super) fn calendar_sql(sql: &str, zone: &str) -> String {
    lexical::rewrite(sql, |sql| calendar_code(sql, zone))
}

fn calendar_code(sql: &str, zone: &str) -> String {
    let mut sql = sql.to_owned();
    for &name in DAILY_VIEWS {
        if !sql.contains(name) {
            continue;
        }
        if let Some(select) = daily_select(name, zone) {
            sql = expand_daily_references(&sql, name, &select, zone);
        }
    }
    // Expansion introduces trusted SQL literals too. Protect them before the
    // function rewrite; original literals/comments stay protected by the outer pass.
    lexical::rewrite(&sql, |sql| calendar_functions(sql, zone))
}

fn calendar_functions(sql: &str, zone: &str) -> String {
    let mut sql = sql.to_owned();
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
    sql = sql.replace(
        "toString(toStartOfFiveMinutes(ts))",
        &format!("toString(toTimeZone(toStartOfFiveMinutes(ts), '{zone}'))"),
    );
    sql = sql.replace("timezone()", &format!("'{zone}'"));
    sql = sql.replace("today()", &format!("toDate(now(), '{zone}')"));
    sql.replace(
        "toStartOfDay(day)",
        &format!("toStartOfDay(toDateTime(day, '{zone}'))"),
    )
}

fn expand_daily_references(sql: &str, name: &str, select: &str, zone: &str) -> String {
    let mut sql = sql.to_owned();
    for keyword in ["FROM", "JOIN"] {
        let marker = format!("{keyword} {name}");
        let mut replaced = String::with_capacity(sql.len());
        let mut cursor = 0;
        for (start, _) in sql.match_indices(&marker) {
            let end = start + marker.len();
            if sql
                .as_bytes()
                .get(end)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                continue;
            }
            let scoped = super::population::reference_scope(name, &sql[end..]).map_or_else(
                || select.to_owned(),
                |predicate| scope::apply(select, predicate, zone),
            );
            replaced.push_str(&sql[cursor..start]);
            let _ = write!(replaced, "{keyword} ({scoped})");
            if !has_alias(&sql[end..]) {
                let _ = write!(replaced, " AS {name}");
            }
            cursor = end;
        }
        replaced.push_str(&sql[cursor..]);
        sql = replaced;
    }
    sql
}

fn has_alias(suffix: &str) -> bool {
    let word = suffix.split_whitespace().next().unwrap_or_default();
    word == "AS"
        || word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && ![
                "WHERE", "GROUP", "ORDER", "LIMIT", "UNION", "FORMAT", "HAVING", "JOIN", "LEFT",
                "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON", "USING",
            ]
            .contains(&word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_recovery_and_aggregate_buckets_use_the_same_display_timezone() {
        for zone in ["America/Los_Angeles", "Asia/Kolkata"] {
            let sql = calendar_sql(
                "SELECT toString(ts5),toString(toStartOfFiveMinutes(ts)) FROM facts",
                zone,
            );
            assert!(sql.contains(&format!("toString(toTimeZone(ts5, '{zone}'))")));
            assert!(sql.contains(&format!(
                "toString(toTimeZone(toStartOfFiveMinutes(ts), '{zone}'))"
            )));
        }
    }

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
            expand_daily_references(
                "FROM mv_user_day_extra UNION ALL SELECT * FROM mv_user_day",
                "mv_user_day",
                "replacement",
                "UTC"
            ),
            "FROM mv_user_day_extra UNION ALL SELECT * FROM (replacement) AS mv_user_day"
        );
    }

    #[test]
    fn preserves_explicit_aliases_on_unproven_references() {
        for alias in ["older", "AS older"] {
            let sql = format!("FROM mv_user_day {alias} WHERE older.user_id=7");
            assert_eq!(
                expand_daily_references(&sql, "mv_user_day", "replacement", "UTC"),
                format!("FROM (replacement) {alias} WHERE older.user_id=7")
            );
        }
    }
}
