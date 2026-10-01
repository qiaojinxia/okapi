//! Versioned call states retain financial events independently. Old unclassified
//! aggregates require complete raw evidence; expiration never invents a call count.
mod scope;
mod spec;

use crate::error::StoreError;
use spec::{CLASSIFIED, Spec};

const HISTORY: &str = "statistics_request_history_incomplete";

// Measurement coverage is a subset of calls, not independent financial truth.
// Its legacy record count cannot override a classified subset or the call parent.
fn canonical(spec: &Spec) -> bool {
    spec.name == "mv_error_hour" || spec.columns.iter().any(|c| c.name == "amount")
}

fn shadow(name: &str) -> String {
    format!("population_v1_{name}")
}

fn source(name: &str) -> String {
    format!("population_source_v2_{name}")
}

fn raw_select(spec: &Spec) -> String {
    let metrics = spec
        .columns
        .iter()
        .map(|c| format!("{} AS {}", c.expression, c.name))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT {},{metrics},countState() AS financial_records,\
        countStateIf({CLASSIFIED}) AS population_classified",
        spec.raw_dimensions
    )
}

fn baseline(spec: &Spec) -> (&str, &str) {
    match spec.name.as_str() {
        "mv_cache_reporting_day" | "mv_cache_write_day" => ("mv_key_model_day", "requests"),
        "mv_calendar_cache_write_hour" | "mv_calendar_cache_reporting_hour" => {
            ("mv_cube_hour", "requests")
        }
        "mv_cache_reporting_hour" => ("mv_analysis_hour", "requests"),
        "mv_error_hour" => ("mv_error_hour", "errors"),
        _ => (&spec.name, "requests"),
    }
}

fn merge(spec: &Spec, alias: &str) -> String {
    spec.columns
        .iter()
        .map(|c| format!("{}({alias}.{}) AS {}", c.merge, c.name, c.name))
        .collect::<Vec<_>>()
        .join(",")
}

fn projection(spec: &Spec, alias: &str, guard: bool) -> String {
    let keys = spec
        .keys
        .iter()
        .map(|k| format!("{alias}.{k} AS {k}"))
        .collect::<Vec<_>>()
        .join(",");
    let metrics = spec
        .columns
        .iter()
        .map(|c| {
            if guard && !c.financial {
                format!(
                    "arrayElement([{alias}.{}],1+throwIf({alias}.n>0,'{HISTORY}')) AS {}",
                    c.name, c.name
                )
            } else {
                format!("{alias}.{} AS {}", c.name, c.name)
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{keys},{metrics},{alias}.financial_records AS financial_records")
}

/// Detach column hashes from recovery subtrees, including reused CTEs.
fn isolate_ctes(mut sql: String) -> String {
    for name in ["p", "a", "e", "missing", "r", "o", "unresolved"] {
        let marker = format!("{name} AS (");
        let Some(start) = sql.find(&marker) else {
            continue;
        };
        let open = start + marker.len() - 1;
        let mut depth = 0_u32;
        let mut quoted = false;
        let close = sql[open..].char_indices().find_map(|(i, ch)| {
            match ch {
                '\'' => quoted = !quoted,
                '(' if !quoted => depth += 1,
                ')' if !quoted => depth = depth.saturating_sub(1),
                _ => {}
            }
            (depth == 0 && !quoted).then_some(open + i)
        });
        if let Some(close) = close {
            let nested = format!("SELECT * FROM view({})", &sql[open + 1..close]);
            sql.replace_range(open + 1..close, &nested);
        }
    }
    sql
}

fn read_view(spec: &Spec) -> String {
    let dims = &spec.dimensions;
    let (parent, count) = baseline(spec);
    let physical = shadow(&spec.name);
    let merge = merge(spec, "v");
    let raw = raw_select(spec);
    let new = projection(spec, "a", false);
    let restored = projection(spec, "r", false);
    let old = projection(spec, "o", true);
    let incomplete_new = projection(spec, "a", true);
    // Each candidate is already grouped once. Joins cannot multiply aggregate
    // states by the number of old blocks. Whole grains are selected, never added.
    isolate_ctes(format!(
        "CREATE VIEW IF NOT EXISTS {} AS WITH \
        p AS (SELECT {dims},countMerge({count}) AS n,countMergeState({count}) AS financial_records FROM {parent} GROUP BY {dims}), \
        a AS (SELECT {dims},{merge},countMergeState(v.financial_records) AS financial_records,\
            countMerge(v.financial_records) AS n,countMerge(v.population_classified) AS classified FROM {physical} v GROUP BY {dims}), \
        e AS (SELECT {dims},max(n) AS expected FROM (SELECT {dims},n FROM p UNION ALL SELECT {dims},n FROM a) GROUP BY {dims}), \
        missing AS (SELECT {dims} FROM e LEFT JOIN a USING ({dims}) WHERE ifNull(a.n,0)!=e.expected OR ifNull(a.classified,0)!=e.expected), \
        r AS ({raw},count() AS n,countIf({CLASSIFIED}) AS classified \
            FROM (SELECT *,{} FROM request_log_raw) AS r INNER JOIN missing USING ({dims}) \
            WHERE {} GROUP BY {dims}), \
        o AS (SELECT {dims},{merge} FROM {} v GROUP BY {dims}), \
        unresolved AS (SELECT {dims} FROM missing INNER JOIN e USING ({dims}) LEFT JOIN r USING ({dims}) \
            WHERE ifNull(r.n,0)<e.expected OR ifNull(r.classified,0)!=ifNull(r.n,0)) \
        SELECT {new} FROM a INNER JOIN e USING ({dims}) WHERE a.n=e.expected AND a.classified=a.n \
        UNION ALL SELECT {restored} FROM r INNER JOIN e USING ({dims}) WHERE r.n>=e.expected AND r.classified=r.n \
        UNION ALL SELECT {old} FROM (SELECT o.*,p.financial_records,p.n AS n FROM o INNER JOIN p USING ({dims})) o \
            INNER JOIN unresolved USING ({dims}) INNER JOIN e USING ({dims}) INNER JOIN p USING ({dims}) WHERE p.n=e.expected \
        UNION ALL SELECT {incomplete_new} FROM a INNER JOIN unresolved USING ({dims}) INNER JOIN e USING ({dims}) \
            LEFT JOIN p USING ({dims}) WHERE ifNull(p.n,0)<e.expected",
        source(&spec.name),
        spec.raw_dimensions,
        spec.predicate,
        spec.name,
    ))
}

pub(super) fn schema() -> Result<Vec<String>, StoreError> {
    let specs = spec::specs()?;
    let mut statements = Vec::new();
    for spec in specs {
        statements.push(format!(
            "{}AS {} FROM request_log_raw AS r WHERE {} GROUP BY {}",
            spec.prefix.replace(&spec.name, &shadow(&spec.name)),
            raw_select(spec),
            spec.predicate,
            spec.dimensions
        ));
    }
    statements.push("CREATE VIEW IF NOT EXISTS request_log_calls AS SELECT * FROM request_log_raw WHERE log_type IN (2,5)".to_owned());
    statements.extend(specs.iter().filter(|s| canonical(s)).map(read_view));
    Ok(statements)
}

/// Rewrite only table identifiers following FROM/JOIN. A single pass prevents
/// recursively replacing the deliberately unclassified evidence in read views.
pub(super) fn referenced(sql: &str) -> Result<Vec<&'static spec::Spec>, StoreError> {
    let specs = spec::specs()?;
    let mut found = Vec::new();
    for (i, _) in sql.match_indices("mv_") {
        let before = &sql[..i];
        if !before.ends_with("FROM ") && !before.ends_with("JOIN ") {
            continue;
        }
        let end = sql[i..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map_or(sql.len(), |n| i + n);
        if let Some(spec) = specs.iter().find(|s| s.name == sql[i..end] && canonical(s))
            && !found.iter().any(|s: &&spec::Spec| s.name == spec.name)
        {
            found.push(spec);
        }
    }
    Ok(found)
}

pub(super) fn coverage(sql: &str, specs: &[&spec::Spec]) -> String {
    let mut branches = Vec::new();
    for spec in specs {
        let (predicate, unscoped) = scope::scopes(sql, spec);
        if unscoped || predicate.is_none() {
            branches.push(coverage_branch(spec, &spec.name, ""));
        }
        if let Some(predicate) = predicate {
            let name = if unscoped {
                format!("{}:scoped", spec.name)
            } else {
                spec.name.clone()
            };
            branches.push(coverage_branch(spec, &name, &format!(" WHERE {predicate}")));
        }
    }
    branches.join(" UNION ALL ")
}

fn coverage_branch(spec: &Spec, name: &str, predicate: &str) -> String {
    let dims = &spec.dimensions;
    let (parent, count) = baseline(spec);
    let physical = shadow(&spec.name);
    format!(
        "SELECT '{name}' AS name,count() AS missing FROM (SELECT {dims},sum(delta) AS difference,sum(unknown) AS unclassified FROM (\
            SELECT {dims},toInt128(countMerge({count})) AS delta,toInt128(0) AS unknown FROM {parent}{predicate} GROUP BY {dims} \
            UNION ALL SELECT {dims},-toInt128(countMerge(financial_records)) AS delta,\
                toInt128(countMerge(financial_records))-toInt128(countMerge(population_classified)) AS unknown FROM {physical}{predicate} GROUP BY {dims}) \
            GROUP BY {dims} HAVING difference>0 OR unclassified!=0)"
    )
}

pub(super) fn sql(sql: &str, complete: &[&str]) -> Result<String, StoreError> {
    let specs = spec::specs()?;
    let mut output = String::with_capacity(sql.len());
    let mut cursor = 0;
    for (i, _) in sql.match_indices("mv_") {
        if !sql[..i].ends_with("FROM ") && !sql[..i].ends_with("JOIN ") {
            continue;
        }
        let end = sql[i..]
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map_or(sql.len(), |n| i + n);
        if let Some(spec) = specs.iter().find(|s| s.name == sql[i..end]) {
            output.push_str(&sql[cursor..i]);
            let name = &sql[i..end];
            let scoped = complete.contains(&format!("{name}:scoped").as_str())
                && scope::reference(&sql[end..], spec).is_some();
            output.push_str(&if complete.contains(&name) || scoped || !canonical(spec) {
                shadow(name)
            } else {
                source(name)
            });
            cursor = end;
        }
    }
    output.push_str(&sql[cursor..]);
    Ok(output)
}
