//! Conservative scope extraction for the trusted generated SQL. Only predicates
//! on stored grain keys can prove a physical source complete. Unsupported syntax
//! retains the whole-table proof; it never grants a narrower fast path.
use super::spec::Spec;

/// A complex reference still needs whole-table proof. Its presence must not
/// force independent references with proven key predicates through recovery.
pub(super) fn scopes(sql: &str, spec: &Spec) -> (Option<String>, bool) {
    let mut predicates = Vec::new();
    let mut unscoped = false;
    for (start, _) in sql.match_indices(&spec.name) {
        let prefix = &sql[..start];
        if !prefix.ends_with("FROM ") && !prefix.ends_with("JOIN ") {
            continue;
        }
        let suffix = &sql[start + spec.name.len()..];
        if suffix.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let Some(predicate) = reference(suffix, spec) else {
            unscoped = true;
            continue;
        };
        let predicate = format!("({predicate})");
        if !predicates.contains(&predicate) {
            predicates.push(predicate);
        }
    }
    (
        (!predicates.is_empty()).then(|| predicates.join(" OR ")),
        unscoped,
    )
}

pub(super) fn reference<'a>(suffix: &'a str, spec: &Spec) -> Option<&'a str> {
    let tail = suffix.trim_start().strip_prefix("WHERE ")?;
    let predicate = &tail[..boundary(tail)?];
    key_predicate(predicate, &spec.keys).then_some(predicate)
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

fn key_predicate(sql: &str, keys: &[String]) -> bool {
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
                if !keys.iter().any(|key| key == word) && !WORDS.contains(&word) {
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

    fn scope(sql: &str) -> Option<String> {
        let specs = super::super::spec::specs().unwrap();
        let (predicate, unscoped) = scopes(
            sql,
            specs.iter().find(|s| s.name == "mv_cube_hour").unwrap(),
        );
        (!unscoped).then_some(predicate).flatten()
    }

    #[test]
    fn every_reference_is_covered_with_bound_primary_filters() {
        assert_eq!(
            scope("SELECT * FROM (SELECT * FROM mv_cube_hour WHERE hour>=toDateTime('2026-09-30') AND model={p_model:String} AND group_code IN ({p_groups_0:String},{p_groups_1:String})) UNION ALL SELECT * FROM mv_cube_hour WHERE user_id=7 GROUP BY user_id UNION ALL SELECT * FROM mv_cube_hour WHERE user_id=7"),
            Some("(hour>=toDateTime('2026-09-30') AND model={p_model:String} AND group_code IN ({p_groups_0:String},{p_groups_1:String})) OR (user_id=7)".to_owned())
        );
    }

    #[test]
    fn aliases_states_comments_and_missing_predicates_keep_global_proof() {
        for sql in [
            "SELECT * FROM mv_cube_hour",
            "SELECT * FROM mv_cube_hour m WHERE m.user_id=7",
            "SELECT * FROM mv_cube_hour WHERE requests>0",
            "SELECT * FROM mv_cube_hour WHERE user_id=7 -- skip predicate",
            "SELECT * FROM mv_cube_hour WHERE user_id=7 /* skip predicate */",
            "SELECT * FROM mv_cube_hour WHERE user_id=7 UNION ALL SELECT * FROM mv_cube_hour",
        ] {
            assert_eq!(scope(sql), None, "{sql}");
        }
    }

    #[test]
    fn quoted_keywords_do_not_end_a_predicate_and_longer_names_are_ignored() {
        assert_eq!(
            scope("SELECT * FROM mv_cube_hour WHERE model='x GROUP BY y\\\'z' ORDER BY hour"),
            Some("(model='x GROUP BY y\\\'z')".to_owned())
        );
        assert_eq!(
            scope("SELECT * FROM mv_cube_hour_backup WHERE user_id=7"),
            None
        );
    }

    #[test]
    fn complex_parent_reference_keeps_separate_coverage() {
        let specs = super::super::spec::specs().unwrap();
        let spec = specs.iter().find(|s| s.name == "mv_cube_hour").unwrap();
        assert_eq!(
            scopes(
                "SELECT * FROM mv_cube_hour WHERE user_id=7 UNION ALL SELECT * FROM mv_cube_hour WHERE (parent_day,parent_user) IN (SELECT day,user_id FROM parents)",
                spec
            ),
            (Some("(user_id=7)".to_owned()), true)
        );
        assert!(reference(" WHERE user_id=7 GROUP BY hour", spec).is_some());
        assert!(reference(" m WHERE m.user_id=7", spec).is_none());
    }
}
