//! 新维度聚合 + 旧立方体未覆盖部分。TTFT 仅在新聚合覆盖不足时恢复原始样本。
const KEYS: &str = "hour, user_id, api_key_id, group_code, model, channel_id";
const DIMS: &str = "requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type";
pub(super) const METRICS: [(&str, &str); 77] = [
    ("legacy_characters", ""),
    ("legacy_character_n", ""),
    ("output_rate_observed", ""),
    ("output_rate_sum", ""),
    ("output_rate_samples", ""),
    ("output_rate_output", ""),
    ("output_rate_known", ""),
    ("output_rate_tokens", ""),
    ("unit_observed", ""),
    ("unit_characters", ""),
    ("unit_character_n", ""),
    ("unit_token_n", ""),
    ("requests", "countMerge"),
    ("financial_records", "countMerge"),
    ("prompt_tokens", "sumMerge"),
    ("cached_tokens", "sumMerge"),
    ("completion_tokens", "sumMerge"),
    ("reasoning_tokens", "sumMerge"),
    ("amount", "sumMerge"),
    ("discount", "sumMerge"),
    ("upstream_cost", "sumMerge"),
    ("errors", "sumMerge"),
    ("latency_sum", "sumMerge"),
    ("latency_samples", "countIfMerge"),
    ("latency_observed", "countMerge"),
    ("latency_output", "sumMerge"),
    ("ttft_sum", "sumMerge"),
    ("ttft_samples", "countIfMerge"),
    ("ttft_observed", "countMerge"),
    ("source_observed", ""),
    ("source_prompt_total", ""),
    ("source_completion_total", ""),
    ("source_cached_total", ""),
    ("source_read_n", ""),
    ("source_prompt_upstream_n", ""),
    ("source_prompt_upstream_tokens", ""),
    ("source_prompt_estimated_n", ""),
    ("source_prompt_estimated_tokens", ""),
    ("source_prompt_local_override_n", ""),
    ("source_prompt_local_override_tokens", ""),
    ("source_completion_upstream_n", ""),
    ("source_completion_upstream_tokens", ""),
    ("source_completion_estimated_n", ""),
    ("source_completion_estimated_tokens", ""),
    ("source_completion_local_override_n", ""),
    ("source_completion_local_override_tokens", ""),
    ("source_cache_n", ""),
    ("source_cache_prompt", ""),
    ("source_cache_read", ""),
    ("cache_observed", ""),
    ("cache_writes", ""),
    ("cache_write_n", ""),
    ("cache_read_n", ""),
    ("ttl_observed", ""),
    ("observed_cache_write_5m_tokens", ""),
    ("observed_cache_write_5m_tokens_n", ""),
    ("observed_cache_write_1h_tokens", ""),
    ("observed_cache_write_1h_tokens_n", ""),
    ("detail_observed", ""),
    ("observed_audio_prompt_tokens", ""),
    ("observed_audio_prompt_tokens_n", ""),
    ("observed_image_prompt_tokens", ""),
    ("observed_image_prompt_tokens_n", ""),
    ("observed_audio_completion_tokens", ""),
    ("observed_audio_completion_tokens_n", ""),
    ("observed_image_completion_tokens", ""),
    ("observed_image_completion_tokens_n", ""),
    ("observed_cache_read_audio_tokens", ""),
    ("observed_cache_read_audio_tokens_n", ""),
    ("observed_cache_read_image_tokens", ""),
    ("observed_cache_read_image_tokens_n", ""),
    ("observed_cache_write_audio_tokens", ""),
    ("observed_cache_write_audio_tokens_n", ""),
    ("observed_cache_write_image_tokens", ""),
    ("observed_cache_write_image_tokens_n", ""),
    ("observed_reasoning_tokens", ""),
    ("observed_reasoning_tokens_n", ""),
];

/// 把括号子查询改成 `view(...)` 表函数，供外层 / join 引用。
///
/// ClickHouse 24.8 的分析器每解析一个引用了子查询列的表达式，都要对该列的来源 `QueryNode`
/// 重新计算树哈希（`ColumnNode` 的哈希带着整棵来源子树），代价 = 表达式个数 × 子查询树大小。
/// 这条源有近百个列表达式引用 `d / l / c` 与十几个测量子源，规划耗时（与数据量无关）可达
/// 数秒，其中 ~90% 在 `getTreeHash`。`view(...)` 在外层分析里只是一张带 schema 的表，哈希
/// 很小；子查询本身仍被完整分析，语义与结果与直接内联完全一致（对照用例逐值核对）。
/// 不是括号子查询的片段原样返回。
fn as_view(subquery: &str) -> String {
    if subquery.starts_with('(') {
        format!("view{subquery}")
    } else {
        subquery.to_owned()
    }
}

/// `window` 和 `scope` 是已校验的 SQL，字符串过滤均由调用方绑定。
pub fn source_with_coverage(
    window: &str,
    scope: &str,
    coverage: super::measurement_coverage::Coverage,
) -> String {
    let predicate = format!("{window}{scope}");
    let detail_keys = format!("{KEYS}, {DIMS}");
    let parent_cache =
        super::cache_usage::prepared(KEYS, "mv_cube_hour", &predicate, coverage.legacy.cache);
    let detail_from = joined_measurements(
        "mv_analysis_hour",
        &detail_keys,
        &predicate,
        coverage.detail,
        coverage.historical_units,
        Some(&parent_cache),
    );
    let legacy_from = joined_measurements(
        "mv_cube_hour",
        KEYS,
        &predicate,
        coverage.legacy,
        coverage.historical_units,
        None,
    );
    let merged = merged_metrics(false, coverage.historical_units);
    let detail_merged = merged_metrics(true, coverage.historical_units);
    let sums = METRICS
        .iter()
        .map(|(name, _)| format!("sum(v_{name}) AS c_{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let detailed = METRICS
        .iter()
        .map(|(name, _)| format!("v_{name} AS {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let remainder = remainder_metrics(coverage.historical_units);
    let legacy_keys = KEYS
        .split(", ")
        .map(|k| format!("l.{k} AS {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let detail_projection = projection(&detail_keys);
    let legacy_projection = projection(KEYS);
    let detail_group = qualified(&detail_keys);
    let legacy_group = qualified(KEYS);
    // 明细 d、旧立方体 l 与按键汇总的明细 c 都以 `view(...)` 引用，不用 CTE 名字：CTE 与内联
    // 子查询一样会让每个引用它的列表达式重复哈希整棵子树（见 `as_view`）。d 在两处引用。
    let d = as_view(&format!(
        "(SELECT {detail_projection}, {detail_merged}, \
            toInt64(countIfMerge(m.cost_known)) AS cost_samples, sumMerge(m.known_amount) AS covered_amount, sumMerge(m.known_cost) AS covered_cost, \
            maxMerge(m.last_event) AS event_at, maxMerge(m.last_ingested) AS ingested_at \
            FROM {detail_from} GROUP BY {detail_group})"
    ));
    let l = as_view(&format!(
        "(SELECT {legacy_projection}, {merged} FROM {legacy_from} GROUP BY {legacy_group})"
    ));
    let c = as_view(&format!("(SELECT {KEYS}, {sums} FROM {d} GROUP BY {KEYS})"));
    format!(
        "(SELECT {KEYS}, {DIMS}, {detailed}, v_cache_writes AS write_tokens, v_cache_write_n AS write_samples, v_cache_read_n AS read_samples, cost_samples, covered_amount, covered_cost, event_at, ingested_at FROM {d} \
        UNION ALL \
        SELECT {legacy_keys}, '' AS requested_model, '' AS upstream_model, '' AS endpoint, '' AS upstream_endpoint, '' AS node, \
            toUInt8(2) AS stream, '' AS request_type, '' AS billing_type, {remainder}, \
            cache_writes AS write_tokens, cache_write_n AS write_samples, cache_read_n AS read_samples, toInt64(0) AS cost_samples, toInt64(0) AS covered_amount, toInt64(0) AS covered_cost, \
            toDateTime64(0, 3) AS event_at, toDateTime64(0, 3) AS ingested_at \
        FROM {l} AS l LEFT JOIN {c} AS c USING ({KEYS}) WHERE l.v_financial_records > ifNull(c.c_financial_records, 0))"
    )
}

fn projection(keys: &str) -> String {
    keys.split(", ")
        .map(|key| format!("m.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn qualified(keys: &str) -> String {
    keys.split(", ")
        .map(|key| format!("m.{key}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn joined_measurements(
    table: &str,
    keys: &str,
    predicate: &str,
    modes: super::measurement_coverage::Modes,
    historical: bool,
    parent_cache: Option<&str>,
) -> String {
    let sources = [
        (
            "td",
            super::token_details::prepared(
                keys,
                table,
                predicate,
                modes.details,
                modes.units,
                modes.ttl,
                historical,
            ),
        ),
        (
            "us",
            super::usage_sources::prepared(keys, table, predicate, modes.usage),
        ),
        (
            "lf",
            super::performance_source::prepared(
                super::performance_source::Kind::Latency,
                keys,
                table,
                predicate,
                modes.latency,
            ),
        ),
        (
            "rf",
            super::output_rate::prepared_with_calibration(
                keys,
                table,
                predicate,
                modes.output_rate,
                historical,
            ),
        ),
        (
            "tf",
            super::performance_source::prepared(
                super::performance_source::Kind::Ttft,
                keys,
                table,
                predicate,
                modes.ttft,
            ),
        ),
        (
            "cs",
            super::cache_usage::prepared(keys, table, predicate, modes.cache),
        ),
    ];
    // Each source has at most one row per grain. Use the primary key directly,
    // instead of recursively merging the same keys through a chain of USING.
    let mut from = format!("(SELECT * FROM {table} WHERE {predicate}) m");
    for (alias, source) in &sources {
        join_measurement(&mut from, source, alias, keys);
    }
    if let Some(source) = parent_cache {
        join_measurement(&mut from, source, "pc", KEYS);
    }
    from
}

fn join_measurement(from: &mut String, source: &str, alias: &str, keys: &str) {
    use std::fmt::Write as _;
    let on = keys
        .split(", ")
        .map(|key| format!("m.{key}={alias}.{key}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let _ = write!(from, " LEFT JOIN {} {alias} ON {on}", as_view(source));
}

fn merged_metrics(modern: bool, historical: bool) -> String {
    METRICS
        .iter()
        .map(|(name, func)| {
            if !historical && matches!(*name,"legacy_characters"|"legacy_character_n") {
                format!("toInt64(0) AS v_{name}")
            } else if *name == "prompt_tokens" && historical {
                let prompt="accurateCast(sumMerge(m.prompt_tokens),'Int64')";
                let characters="max(ifNull(td.legacy_characters,0))";
                format!("{prompt}-{characters}+toInt64(throwIf({characters}>{prompt})) AS v_prompt_tokens")
            } else if *name == "source_prompt_total" && historical {
                // A partial provenance subtotal may not contain the old record.
                // Only a complete chosen population admits an exact subtraction.
                format!("toInt64(max(ifNull(us.{name},0)))-if(max(ifNull(us.source_observed,0))=countMerge(m.requests),toInt64(max(ifNull(td.legacy_characters,0))),toInt64(0)) AS v_{name}")
            } else if let Some(source) = super::observation_sources::owner(name) {
                if source.kind == super::observation_sources::Kind::Cache {
                    cache_metric(name, modern)
                } else {
                    let alias = source.alias;
                    format!("toInt64(max(ifNull({alias}.{name}, 0))) AS v_{name}")
                }
            } else {
                format!("toInt64({func}(m.{name})) AS v_{name}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn cache_metric(name: &str, modern: bool) -> String {
    let own = format!("max(ifNull(cs.{name}, 0))");
    let complete = |axis: &str| {
        format!(
            "max(ifNull(pc.cache_expected, 0)) >= countMerge(m.requests) AND max(ifNull(pc.cache_expected, 0)) > 0 AND max(ifNull(pc.cache_{axis}_n, 0)) = max(ifNull(pc.cache_expected, 0))"
        )
    };
    let expression = if modern {
        match name {
            "cache_write_n" => format!(
                "if({}, toInt64(countMerge(m.requests)), toInt64({own}))",
                complete("write")
            ),
            "cache_read_n" => format!(
                "if({}, toInt64(countMerge(m.requests)), toInt64({own}))",
                complete("read")
            ),
            "cache_writes" => format!(
                "if({} OR max(ifNull(cs.cache_write_n, 0)) = countMerge(m.requests), toInt64(sumMerge(m.writes)), toInt64({own}))",
                complete("write")
            ),
            _ => format!(
                "if(({}) AND ({}), toInt64(countMerge(m.requests)), toInt64({own}))",
                complete("write"),
                complete("read")
            ),
        }
    } else {
        own
    };
    format!("toInt64({expression}) AS v_{name}")
}

fn cache_remainder(name: &str, difference: &str) -> String {
    let observed = "l.v_cache_observed = l.v_requests AND ifNull(c.c_cache_observed, 0) = ifNull(c.c_requests, 0)";
    let complete = |axis: &str| {
        format!(
            "l.v_cache_{axis}_n = l.v_requests AND ifNull(c.c_cache_{axis}_n, 0) = ifNull(c.c_requests, 0)"
        )
    };
    let covered = match name {
        "cache_write_n" | "cache_writes" => format!("({observed}) OR ({})", complete("write")),
        "cache_read_n" => format!("({observed}) OR ({})", complete("read")),
        _ => observed.to_owned(),
    };
    format!(
        "if(({covered}) AND l.v_{name} >= ifNull(c.c_{name}, 0), {difference}, toInt64(0)) AS {name}"
    )
}

fn remainder_metrics(historical: bool) -> String {
    use super::observation_sources::{Kind, owner};
    METRICS.iter().map(|(name, _)| {
        let difference = format!("l.v_{name} - ifNull(c.c_{name}, 0)");
        if !historical && matches!(*name,"legacy_characters"|"legacy_character_n") {
            return format!("toInt64(0) AS {name}");
        }
        match owner(name).map(|source| source.kind) {
            Some(Kind::OutputRate) => rate_remainder(name, &difference,historical),
            Some(Kind::Ttft | Kind::Latency) => {
                let prefix = owner(name).unwrap().name;
                format!("if(l.v_{prefix}_observed = l.v_requests AND ifNull(c.c_{prefix}_observed, 0) = ifNull(c.c_requests, 0), {difference}, toInt64(0)) AS {name}")
            }
            Some(Kind::Cache) => cache_remainder(name, &difference),
            Some(Kind::Units) => unit_remainder(name, &difference,historical),
            Some(Kind::Ttl) => format!("if(l.v_ttl_observed = l.v_requests AND ifNull(c.c_ttl_observed, 0) = ifNull(c.c_requests, 0) AND l.v_{name} >= ifNull(c.c_{name},0), {difference}, toInt64(0)) AS {name}"),
            Some(Kind::Details) => format!("if(l.v_detail_observed = l.v_requests AND ifNull(c.c_detail_observed, 0) = ifNull(c.c_requests, 0), {difference}, toInt64(0)) AS {name}"),
            Some(Kind::Usage) => format!("if(l.v_source_observed = l.v_requests AND ifNull(c.c_source_observed, 0) = ifNull(c.c_requests, 0), {difference}, toInt64(0)) AS {name}"),
            None => format!("{difference} AS {name}"),
        }
    }).collect::<Vec<_>>().join(", ")
}

fn rate_remainder(name: &str, difference: &str, historical: bool) -> String {
    let fallback = if historical && matches!(name, "output_rate_observed" | "output_rate_known") {
        "l.v_legacy_character_n-ifNull(c.c_legacy_character_n,0)"
    } else {
        "toInt64(0)"
    };
    format!(
        "if(l.v_output_rate_observed=l.v_requests AND ifNull(c.c_output_rate_observed,0)=ifNull(c.c_requests,0) AND l.v_{name}>=ifNull(c.c_{name},0),{difference},{fallback}) AS {name}"
    )
}

fn unit_remainder(name: &str, difference: &str, historical: bool) -> String {
    // Partial generic metadata cannot be subtracted. Independent historical
    // proof has the same identities in both scopes and can retain its residual.
    let fallback = if historical {
        match name {
            "unit_observed" | "unit_character_n" => {
                "l.v_legacy_character_n-ifNull(c.c_legacy_character_n,0)".to_owned()
            }
            "unit_characters" => "l.v_legacy_characters-ifNull(c.c_legacy_characters,0)".to_owned(),
            "legacy_characters" | "legacy_character_n" => difference.to_owned(),
            _ => "toInt64(0)".to_owned(),
        }
    } else {
        "toInt64(0)".to_owned()
    };
    let complete =
        "l.v_unit_observed=l.v_requests AND ifNull(c.c_unit_observed,0)=ifNull(c.c_requests,0)";
    format!(
        "if(({complete}) AND l.v_{name}>=ifNull(c.c_{name},0),{difference},{fallback}) AS {name}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::measurement_coverage::{Coverage, Mode, Modes};

    fn balanced(sql: &str) -> bool {
        let mut depth = 0_i64;
        for ch in sql.chars() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                return false;
            }
        }
        depth == 0
    }

    #[test]
    fn as_view_only_wraps_parenthesised_subqueries() {
        assert_eq!(as_view("(SELECT 1)"), "view(SELECT 1)");
        assert_eq!(as_view("mv_cube_hour"), "mv_cube_hour");
    }

    #[test]
    fn structural_sources_are_table_functions_not_cte_or_inline_subqueries() {
        let aggregate = Modes {
            ttft: Mode::Aggregate,
            latency: Mode::Aggregate,
            usage: Mode::Aggregate,
            details: Mode::Aggregate,
            ttl: Mode::Aggregate,
            cache: Mode::Aggregate,
            units: Mode::Aggregate,
            output_rate: Mode::Aggregate,
        };
        for coverage in [
            Coverage::default(),
            Coverage {
                detail: aggregate,
                legacy: aggregate,
                historical_units: false,
            },
            Coverage {
                historical_units: true,
                ..Coverage::default()
            },
        ] {
            let sql = source_with_coverage(
                "hour >= toDateTime('2026-09-01')",
                " AND user_id = 7",
                coverage,
            );
            assert!(sql.starts_with("(SELECT "), "{}", &sql[..40]);
            assert!(balanced(&sql));
            // 近百个列表达式引用 d / l / c 与测量子源：任何一个退回 CTE 或括号子查询都会让规划耗时数倍。
            assert!(
                !sql.contains("WITH d AS") && !sql.contains("l AS ("),
                "不得使用 CTE"
            );
            // 本层的 join：每个测量子源（`ON m.hour=…`）加上 `l LEFT JOIN c` 都必须是 view(...)。
            // 子源内部（别的模块生成的 e / a / r 等）的普通 join 不在此列。
            assert_eq!(
                sql.matches(" LEFT JOIN view(").count(),
                sql.matches(" ON m.hour=").count() + 1,
                "测量子源必须以 view(...) 引用"
            );
            assert!(
                sql.contains(" AS l LEFT JOIN view(SELECT "),
                "l 与 c 必须以 view(...) 引用"
            );
            assert!(sql.contains(
                ") AS c USING (hour, user_id, api_key_id, group_code, model, channel_id)"
            ));
            assert!(
                sql.matches("FROM view(SELECT ").count() >= 2,
                "d 在 UNION 第一支与 c 中各引用一次"
            );
            assert!(
                sql.contains("WHERE l.v_financial_records > ifNull(c.c_financial_records, 0))")
            );
        }
    }
}
