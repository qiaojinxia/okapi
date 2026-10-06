//! Keep provable outer scope on retained sources before calendar expansion.
use super::super::population;
use std::fmt::Write as _;

pub(super) fn apply(sql: &str, predicate: &str, zone: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut cursor = 0;
    for (start, _) in sql.match_indices("mv_") {
        if !sql[..start].ends_with("FROM ") && !sql[..start].ends_with("JOIN ") {
            continue;
        }
        let end = sql[start..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map_or(sql.len(), |length| start + length);
        let name = &sql[start..end];
        let Some(time) = population::calendar_time_key(name) else {
            continue;
        };
        let first = if time == "day" {
            "toDateTime(day,'UTC')".to_owned()
        } else {
            format!("toStartOfDay({time},'UTC')")
        };
        // Day recovery compares a complete UTC day with its retained hours.
        // Clip all precisions to the same UTC-day envelope, never partial hours/days.
        let low = replace_day(predicate, &format!("toDate({first},'{zone}')"));
        let high = replace_day(
            predicate,
            &format!("toDate({first}+INTERVAL 86399 SECOND,'{zone}')"),
        );
        let scope = if low == high {
            low
        } else {
            format!("({low}) OR ({high})")
        };
        if !population::valid_scope(name, &scope) {
            continue;
        }
        result.push_str(&sql[cursor..start]);
        let _ = write!(result, "(SELECT * FROM {name} WHERE {scope})");
        cursor = end;
    }
    result.push_str(&sql[cursor..]);
    result
}

// Skip string literals and bound parameters, including parameters named `day`.
fn replace_day(sql: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                    } else if bytes[i] == b'\'' {
                        i += 1;
                        if bytes.get(i) == Some(&b'\'') {
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'{' => {
                i = sql[i..].find('}').map_or(bytes.len(), |end| i + end + 1);
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                i += 1;
                while bytes
                    .get(i)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    i += 1;
                }
                if &sql[start..i] == "day" {
                    result.push_str(replacement);
                    continue;
                }
            }
            _ => i += sql[i..].chars().next().map_or(1, char::len_utf8),
        }
        result.push_str(&sql[start..i]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_the_date_column_and_keeps_literals_and_bindings() {
        let predicate = "day={day:Date} AND model='day \\'quoted' AND user_id={user:Int64}";
        let rewritten = replace_day(predicate, "local_day");
        assert!(rewritten.starts_with("local_day={day:Date}"));
        assert!(rewritten.contains("model='day \\'quoted'"));
        assert!(rewritten.contains("user_id={user:Int64}"));
    }

    #[test]
    fn calendar_scope_is_a_conservative_superset_of_intersecting_buckets() {
        let sql = apply(
            "SELECT * FROM mv_calendar_minute UNION ALL SELECT * FROM mv_cube_hour UNION ALL SELECT * FROM mv_user_day",
            "user_id={user:Int64} AND day={day:Date}",
            "Asia/Kolkata",
        );
        assert!(sql.contains("toStartOfDay(minute,'UTC')"));
        assert!(sql.contains("toStartOfDay(hour,'UTC')"));
        assert_eq!(sql.matches("INTERVAL 86399 SECOND").count(), 3);
        assert!(sql.contains("toDateTime(day,'UTC')"));
        assert_eq!(sql.matches("user_id={user:Int64}").count(), 6);
        let unsupported = "SELECT * FROM mv_cube_hour";
        assert_eq!(
            apply(unsupported, "requests>0", "Asia/Kolkata"),
            unsupported
        );
    }
}
