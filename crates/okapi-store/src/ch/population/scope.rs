//! Conservative scope extraction for trusted generated SQL. Only predicates
//! on stored grain keys can be pushed down during calendar view expansion.

pub(super) fn reference<'a>(suffix: &'a str, keys: &[&str]) -> Option<&'a str> {
    let tail = suffix.trim_start().strip_prefix("WHERE ")?;
    let predicate = &tail[..boundary(tail)?];
    key_predicate(predicate, keys).then_some(predicate)
}

fn quoted_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'\'' if bytes.get(i + 1) == Some(&b'\'') => i += 2,
            b'\'' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn boundary(sql: &str) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut depth = 0_usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                i = quoted_end(bytes, i)?;
                continue;
            }
            b'(' => depth += 1,
            b')' | b';' if depth == 0 => return Some(i),
            b')' => depth -= 1,
            _ => {}
        }
        if depth == 0
            && bytes[i].is_ascii_whitespace()
            && [
                "GROUP BY ",
                "ORDER BY ",
                "LIMIT ",
                "UNION ",
                "FORMAT ",
                "HAVING ",
            ]
            .iter()
            .any(|keyword| sql[i..].trim_start().starts_with(keyword))
        {
            return Some(i);
        }
        i += 1;
    }
    (depth == 0).then_some(bytes.len())
}

pub(super) fn key_predicate(sql: &str, keys: &[&str]) -> bool {
    const WORDS: &[&str] = &[
        "AND",
        "OR",
        "NOT",
        "IN",
        "BETWEEN",
        "IS",
        "NULL",
        "INTERVAL",
        "DAY",
        "HOUR",
        "MINUTE",
        "SECOND",
        "TRUE",
        "FALSE",
        "LIKE",
        "toDate",
        "toDateTime",
        "toDateTime64",
        "toStartOfDay",
        "now",
        "today",
        "now64",
    ];
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                let Some(end) = quoted_end(bytes, i) else {
                    return false;
                };
                i = end;
            }
            b'{' => {
                let Some(end) = sql[i..].find('}') else {
                    return false;
                };
                let placeholder = &sql[i + 1..i + end];
                if !placeholder.contains(':') || placeholder.contains('{') {
                    return false;
                }
                i += end + 1;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = i;
                while bytes
                    .get(i)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    i += 1;
                }
                let word = &sql[start..i];
                if !keys.contains(&word) && !WORDS.contains(&word) {
                    return false;
                }
            }
            b'.' | b'`' | b'"' | b'#' | b'}' => return false,
            b'-' if bytes.get(i + 1) == Some(&b'-') => return false,
            b'/' if bytes.get(i + 1) == Some(&b'*') => return false,
            b if !b.is_ascii() => return false,
            _ => i += 1,
        }
    }
    !sql.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bound_key_predicates_and_ignores_aliases_or_metric_filters() {
        let keys = &["user_id", "day"];
        assert_eq!(
            reference(
                " WHERE user_id={user:Int64} AND day={day:Date} GROUP BY day",
                keys
            ),
            Some("user_id={user:Int64} AND day={day:Date}")
        );
        assert_eq!(reference(" m WHERE m.user_id=7", keys), None);
        assert_eq!(reference(" WHERE requests>0", keys), None);
        assert_eq!(reference(" WHERE user_id=7 -- comment", keys), None);
        assert_eq!(
            reference(
                " WHERE model='x GROUP BY y\\\'z' ORDER BY day",
                &["model", "day"]
            ),
            Some("model='x GROUP BY y\\\'z'")
        );
    }
}
