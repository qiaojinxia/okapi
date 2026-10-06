//! Preserve literal, identifier and comment spans while rewriting SQL code.
//! This is a lexical boundary, not a SQL parser or a parameter interpolator.
pub(super) fn rewrite(sql: &str, transform: impl FnOnce(&str) -> String) -> String {
    let mut prefix = "__okapi_protected_".to_owned();
    while sql.contains(&prefix) {
        prefix.push('_');
    }
    let bytes = sql.as_bytes();
    let mut masked = String::with_capacity(sql.len());
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let quoted = matches!(bytes[i], b'\'' | b'"' | b'`');
        let comment =
            bytes[i] == b'#' || bytes[i..].starts_with(b"--") || bytes[i..].starts_with(b"/*");
        if quoted {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    if bytes.get(i) == Some(&quote) {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if comment {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            i += sql[i..].chars().next().map_or(1, char::len_utf8);
            masked.push_str(&sql[start..i]);
            continue;
        }
        let token = format!("{prefix}{}__", spans.len());
        let token = if quoted {
            let q = char::from(bytes[start]);
            format!("{q}{token}{q}")
        } else {
            format!("/*{token}*/")
        };
        masked.push_str(&token);
        spans.push((token, &sql[start..i]));
    }
    let mut rewritten = transform(&masked);
    for (token, original) in spans {
        rewritten = rewritten.replace(&token, original);
    }
    rewritten
}

#[cfg(test)]
mod tests {
    #[test]
    fn protects_every_non_code_span_and_handles_escaped_quotes() {
        let sql = "SELECT today(), 'today() \\' timezone()', \"today()\", `timezone()`, 'it''s today()' -- today()\n/* outer /* today() */ timezone() */ # today()\n";
        let result = super::rewrite(sql, |code| code.replace("today()", "local_date()"));
        assert_eq!(result, sql.replacen("today()", "local_date()", 1));
    }
}
