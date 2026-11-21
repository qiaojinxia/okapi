//! Parse the embedded, trusted schema; this is not a parser for user SQL.
use crate::error::StoreError;

#[cfg(test)]
const CLASSIFIED: &str = "log_type IN (2, 5, 6)";

pub(in crate::ch) struct Column {
    pub name: String,
    pub merge: String,
    pub financial: bool,
}

pub(in crate::ch) struct Spec {
    pub name: String,
    pub dimensions: String,
    pub keys: Vec<String>,
    pub columns: Vec<Column>,
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

/// Map an MV aggregate state expression to its corresponding `MergeState` combinator.
///
/// In ClickHouse:
/// - `FState(...)` and `FStateIf(..., cond)` store `AggregateFunction(F, ...)` -> `FMergeState`
/// - `FIfState(..., cond)` stores `AggregateFunction(FIf, ...)` -> `FIfMergeState`
/// - Parameterized functions like `quantilesIfState(0.5, 0.95, 0.99)(...)` keep their parameter list.
fn aggregate(expression: &str, name: &str) -> Result<Column, StoreError> {
    let open = expression.find('(').ok_or_else(invalid)?;
    let function = &expression[..open];
    let state_fn = if function.ends_with("StateIf") {
        &function[..function.len() - 2]
    } else if function.ends_with("State") {
        function
    } else {
        return Err(invalid());
    };
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
            | "financial_records"
    );
    Ok(Column {
        name: name.to_owned(),
        merge: format!("{}{parameters}", state_fn.replace("State", "MergeState")),
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
        let (_, group) = tail.split_once("GROUP BY").ok_or_else(invalid)?;
        let keys = comma_parts(group)
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        let mut columns = Vec::new();
        for part in comma_parts(projection) {
            if let Some((expression, alias)) = part.rsplit_once(" AS ")
                && expression
                    .split('(')
                    .next()
                    .is_some_and(|f| f.ends_with("State") || f.ends_with("StateIf"))
            {
                let alias = alias.trim();
                if alias != "population_classified" {
                    columns.push(aggregate(expression.trim(), alias)?);
                }
            }
        }
        specs.push(Spec {
            name: name.to_owned(),
            dimensions: keys.join(","),
            keys,
            columns,
        });
    }
    Ok(specs)
}

/// Logical daily view keys used by calendar predicate pushdown.
pub(super) fn daily_keys(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "mv_user_day" => Some(&["user_id", "day"]),
        "mv_apikey_day" => Some(&["api_key_id", "day"]),
        "mv_group_day" => Some(&["group_code", "day"]),
        "mv_user_model_day" => Some(&["user_id", "model", "day"]),
        "mv_key_model_day" | "mv_cache_write_day" | "mv_cache_reporting_day" => {
            Some(&["user_id", "api_key_id", "model", "day"])
        }
        "mv_client_day" => Some(&["client_type", "day"]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classified_filter_matches_the_delivery_whitelist() {
        let listed: Vec<String> = crate::ch::STATISTICS_LOG_TYPES
            .iter()
            .map(ToString::to_string)
            .collect();
        let expected = format!("log_type IN ({})", listed.join(", "));
        assert_eq!(super::CLASSIFIED, expected);
        assert!(include_str!("../../ch_schema.sql").contains(&expected));
    }

    #[test]
    fn merge_states_preserve_parameters_and_financial_classification() {
        assert_eq!(
            aggregate("countStateIf(log_type IN (2, 5))", "requests")
                .unwrap()
                .merge,
            "countMergeState"
        );
        let quantile = aggregate(
            "quantilesIfState(0.5, 0.95, 0.99)(latency_ms, log_type IN (2, 5))",
            "latency_q",
        )
        .unwrap();
        assert_eq!(quantile.merge, "quantilesIfMergeState(0.5, 0.95, 0.99)");
        assert!(!quantile.financial);
        let amount = aggregate("sumState(amount_micro)", "amount").unwrap();
        assert_eq!(amount.merge, "sumMergeState");
        assert!(amount.financial);
        let specs = specs().unwrap();
        assert_eq!(specs.len(), 5, "converged schema must stay at 5 core MVs");
        for name in [
            "mv_analysis_5min",
            "mv_calendar_minute",
            "mv_model_hour",
            "mv_channel_5min",
            "mv_error_hour",
        ] {
            assert!(specs.iter().any(|s| s.name == name), "missing {name}");
        }
    }
}
