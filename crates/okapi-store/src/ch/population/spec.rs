//! Parse the embedded, trusted schema; this is not a parser for user SQL.
use crate::error::StoreError;

pub(super) const CALL: &str = "log_type IN (2,5)";
pub(super) const CLASSIFIED: &str = "log_type IN (2,5,6)";

pub(in crate::ch) struct Column {
    pub name: String,
    pub expression: String,
    pub merge: String,
    pub financial: bool,
}

pub(in crate::ch) struct Spec {
    pub name: String,
    pub prefix: String,
    pub dimensions: String,
    pub keys: Vec<String>,
    pub columns: Vec<Column>,
    pub raw_dimensions: String,
    pub predicate: String,
}

fn invalid() -> StoreError {
    StoreError::InvalidData("statistics_population_schema_invalid")
}

fn comma_parts(input: &str) -> Vec<&str> {
    let mut depth = 0_i32;
    let mut quoted = false;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, byte) in input.bytes().enumerate() {
        match byte {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth -= 1,
            b',' if !quoted && depth == 0 => {
                parts.push(input[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(input[start..].trim());
    parts
}

/// `StateIf` preserves the nested state type, unlike replacing `countState`
/// with `countIfState`. Verified against the supported ClickHouse 24.8 runtime.
fn aggregate(expression: &str, name: &str) -> Result<Column, StoreError> {
    let open = expression.find('(').ok_or_else(invalid)?;
    let function = &expression[..open];
    if !function.ends_with("State") {
        return Err(invalid());
    }
    let mut depth = 0_i32;
    let close = expression[open..]
        .char_indices()
        .find_map(|(i, ch)| {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(open + i)
        })
        .ok_or_else(invalid)?;
    let parameters = if close + 1 == expression.len() {
        ""
    } else {
        &expression[open..=close]
    };
    let financial = matches!(
        name,
        "amount"
            | "original"
            | "discount"
            | "upstream_cost"
            | "cost_known"
            | "known_amount"
            | "known_cost"
            | "last_event"
            | "last_ingested"
    );
    let call_expression = if financial {
        expression.to_owned()
    } else {
        let args = if parameters.is_empty() {
            open
        } else {
            close + 1
        };
        let separator = if expression[args + 1..expression.len() - 1].trim().is_empty() {
            ""
        } else {
            ","
        };
        format!(
            "{function}If{}{separator}({CALL}))",
            &expression[open..expression.len() - 1]
        )
    };
    Ok(Column {
        name: name.to_owned(),
        expression: call_expression,
        merge: format!("{}{parameters}", function.replace("State", "MergeState")),
        financial,
    })
}

pub(super) fn specs() -> Result<&'static [Spec], StoreError> {
    static SPECS: std::sync::OnceLock<Option<Vec<Spec>>> = std::sync::OnceLock::new();
    SPECS
        .get_or_init(|| parse().ok())
        .as_deref()
        .ok_or_else(invalid)
}

fn parse() -> Result<Vec<Spec>, StoreError> {
    let schema = include_str!("../../ch_schema.sql").replace('\r', "");
    let mut specs = Vec::new();
    for statement in schema.split(";\n") {
        let Some(start) = statement.find("CREATE MATERIALIZED VIEW IF NOT EXISTS ") else {
            continue;
        };
        let ddl = &statement[start..];
        let (prefix, select) = ddl.split_once("AS SELECT").ok_or_else(invalid)?;
        let name = prefix.split_whitespace().nth(6).ok_or_else(invalid)?;
        let (projection, tail) = select
            .split_once("FROM request_log_raw")
            .ok_or_else(invalid)?;
        let (tail, group) = tail.split_once("GROUP BY").ok_or_else(invalid)?;
        let keys = comma_parts(group)
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        let predicate = tail
            .split_once("WHERE")
            .map_or("1", |(_, p)| p.trim())
            .to_owned();
        let mut dimensions = Vec::new();
        let mut columns = Vec::new();
        for part in comma_parts(projection) {
            if let Some((expression, alias)) = part.rsplit_once(" AS ")
                && expression
                    .split('(')
                    .next()
                    .is_some_and(|f| f.ends_with("State"))
            {
                columns.push(aggregate(expression.trim(), alias.trim())?);
                continue;
            }
            dimensions.push(part);
        }
        specs.push(Spec {
            name: name.to_owned(),
            prefix: prefix.to_owned(),
            dimensions: keys.join(","),
            keys,
            columns,
            raw_dimensions: dimensions.join(","),
            predicate,
        });
    }
    Ok(specs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditional_states_preserve_parameters_and_financial_sums() {
        assert_eq!(
            aggregate("countState()", "requests").unwrap().expression,
            "countStateIf((log_type IN (2,5)))"
        );
        let quantile = aggregate("quantilesState(0.5,0.95)(latency_ms)", "latency_q").unwrap();
        assert_eq!(
            quantile.expression,
            "quantilesStateIf(0.5,0.95)(latency_ms,(log_type IN (2,5)))"
        );
        assert_eq!(quantile.merge, "quantilesMergeState(0.5,0.95)");
        assert_eq!(
            aggregate("sumState(amount_micro)", "amount")
                .unwrap()
                .expression,
            "sumState(amount_micro)"
        );
        assert!(
            specs()
                .unwrap()
                .iter()
                .any(|s| s.name == "mv_calendar_minute")
        );
    }
}
