//! Zero-shadow population guards: each canonical MV classifies `log_type IN (2,5)`
//! on insert and tracks `population_classified` alongside `financial_records`.
//! Lightweight logical guard views fail closed with `statistics_request_history_incomplete`
//! when any unclassified event (`log_type NOT IN (2,5,6)`) enters the queried scope,
//! while still permitting financial (`amount`) queries without shadow MVs or pre-probes.
mod scope;
mod spec;

use crate::error::StoreError;
use spec::Spec;

const HISTORY: &str = "statistics_request_history_incomplete";

pub(super) fn reference_scope<'a>(name: &str, suffix: &'a str) -> Option<&'a str> {
    if let Some(keys) = spec::daily_keys(name) {
        return scope::reference(suffix, keys);
    }
    let spec = spec::specs().ok()?.iter().find(|spec| spec.name == name)?;
    let keys: Vec<&str> = spec.keys.iter().map(String::as_str).collect();
    scope::reference(suffix, &keys)
}

pub(super) fn valid_scope(name: &str, predicate: &str) -> bool {
    if let Some(keys) = spec::daily_keys(name) {
        return scope::key_predicate(predicate, keys);
    }
    spec::specs().is_ok_and(|specs| {
        specs
            .iter()
            .find(|spec| spec.name == name)
            .is_some_and(|spec| {
                let keys: Vec<&str> = spec.keys.iter().map(String::as_str).collect();
                scope::key_predicate(predicate, &keys)
            })
    })
}

pub(super) fn calendar_time_key(name: &str) -> Option<&'static str> {
    if spec::daily_keys(name).is_some() {
        return Some("day");
    }
    let spec = spec::specs().ok()?.iter().find(|spec| spec.name == name)?;
    ["minute", "ts5", "hour", "day"]
        .into_iter()
        .find(|key| spec.keys.iter().any(|stored| stored == key))
}

fn guard_view_name(name: &str) -> String {
    format!("population_guard_{name}")
}

fn guard_view(spec: &Spec) -> String {
    let dims = &spec.dimensions;
    let merge = spec
        .columns
        .iter()
        .map(|c| format!("{}(v.{}) AS {}", c.merge, c.name, c.name))
        .collect::<Vec<_>>()
        .join(",");
    let outer_cols = spec
        .columns
        .iter()
        .map(|c| {
            if c.financial {
                format!("{} AS {}", c.name, c.name)
            } else {
                format!(
                    "arrayElement([{}],1+throwIf(classified_n<financial_n,'{HISTORY}')) AS {}",
                    c.name, c.name
                )
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "CREATE OR REPLACE VIEW {} AS \
         SELECT {dims},{outer_cols} FROM (\
             SELECT {dims},{merge},\
                 countMerge(v.financial_records) AS financial_n,\
                 countMerge(v.population_classified) AS classified_n \
             FROM {} AS v GROUP BY {dims}\
         )",
        guard_view_name(&spec.name),
        spec.name,
    )
}

pub(super) fn read_views() -> Result<Vec<String>, StoreError> {
    let specs = spec::specs()?;
    let mut views = Vec::with_capacity(specs.len() + 1);
    views.push(
        "CREATE OR REPLACE VIEW request_log_calls AS SELECT * FROM request_log_raw WHERE log_type IN (2,5)"
            .to_owned(),
    );
    views.extend(specs.iter().map(guard_view));
    Ok(views)
}

/// Rewrite canonical MV table identifiers following `FROM ` / `JOIN ` to their
/// zero-storage logical guard views in a single pass.
pub(super) fn sql(sql: &str) -> Result<String, StoreError> {
    let specs = spec::specs()?;
    let mut output = String::with_capacity(sql.len() + 64);
    let mut cursor = 0;
    for (i, _) in sql.match_indices("mv_") {
        if !sql[..i].ends_with("FROM ") && !sql[..i].ends_with("JOIN ") {
            continue;
        }
        let end = sql[i..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map_or(sql.len(), |n| i + n);
        let name = &sql[i..end];
        if specs.iter().any(|s| s.name == name) {
            output.push_str(&sql[cursor..i]);
            output.push_str(&guard_view_name(name));
            cursor = end;
        }
    }
    output.push_str(&sql[cursor..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_canonical_mv_references_to_guard_views_without_shadow_mvs() {
        let views = read_views().unwrap();
        assert_eq!(views.len(), 6);
        assert!(
            views
                .iter()
                .all(|v| !v.contains("CREATE MATERIALIZED VIEW"))
        );
        assert!(views.iter().all(|v| !v.contains("population_v1_")));
        let rewritten =
            sql("SELECT countMerge(requests) FROM mv_analysis_5min WHERE user_id = 7").unwrap();
        assert!(rewritten.contains("FROM population_guard_mv_analysis_5min "));
        let untouched = sql("SELECT * FROM mv_analysis_5min_backup").unwrap();
        assert_eq!(untouched, "SELECT * FROM mv_analysis_5min_backup");
    }
}
