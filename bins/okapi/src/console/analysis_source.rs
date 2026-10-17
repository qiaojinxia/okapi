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
///
/// 只读明细聚合 `mv_analysis_hour`。它与旧立方体 `mv_cube_hour` 都由 `request_log_raw` 无条件写入、
/// 都不过期，旧立方体比明细多出来的部分只可能是明细上线前的历史；开发阶段不兼容旧库，所以不再
/// 拼「旧立方体剩余」那一支——它要把 `d` 再汇总一遍并 join 一整套测量子源，规划耗时是现在的 4 倍。
pub fn source_with_coverage(
    window: &str,
    scope: &str,
    coverage: super::measurement_coverage::Coverage,
) -> String {
    let predicate = format!("{window}{scope}");
    let detail_keys = format!("{KEYS}, {DIMS}");
    let parent_cache = super::cache_usage::source(KEYS, &predicate);
    let detail_from = joined_measurements(
        "mv_analysis_hour",
        &detail_keys,
        &predicate,
        coverage.historical_units,
        Some(&parent_cache),
    );
    let detail_merged = merged_metrics(coverage.historical_units);
    let detailed = METRICS
        .iter()
        .map(|(name, _)| format!("v_{name} AS {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let detail_projection = projection(&detail_keys);
    let detail_group = qualified(&detail_keys);
    // d 以 `view(...)` 引用，不用 CTE 名字：CTE 与内联子查询一样会让每个引用它的列表达式重复哈希
    // 整棵子树（见 `as_view`）。
    let d = as_view(&format!(
        "(SELECT {detail_projection}, {detail_merged}, \
            toInt64(countIfMerge(m.cost_known)) AS cost_samples, sumMerge(m.known_amount) AS covered_amount, sumMerge(m.known_cost) AS covered_cost, \
            maxMerge(m.last_event) AS event_at, maxMerge(m.last_ingested) AS ingested_at \
            FROM {detail_from} GROUP BY {detail_group})"
    ));
    format!(
        "(SELECT {KEYS}, {DIMS}, {detailed}, v_cache_writes AS write_tokens, v_cache_write_n AS write_samples, v_cache_read_n AS read_samples, cost_samples, covered_amount, covered_cost, event_at, ingested_at FROM {d})"
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
    historical: bool,
    parent_cache: Option<&str>,
) -> String {
    let sources = [
        (
            "td",
            super::token_details::prepared(keys, table, predicate, historical),
        ),
        ("us", super::usage_sources::source(keys, predicate)),
        (
            "lf",
            super::performance_source::source(
                super::performance_source::Kind::Latency,
                keys,
                predicate,
            ),
        ),
        (
            "rf",
            super::output_rate::prepared_with_calibration(keys, table, predicate, historical),
        ),
        (
            "tf",
            super::performance_source::source(
                super::performance_source::Kind::Ttft,
                keys,
                predicate,
            ),
        ),
        ("cs", super::cache_usage::source(keys, predicate)),
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

fn merged_metrics(historical: bool) -> String {
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
                    cache_metric(name)
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

fn cache_metric(name: &str) -> String {
    let own = format!("max(ifNull(cs.{name}, 0))");
    let complete = |axis: &str| {
        format!(
            "max(ifNull(pc.cache_expected, 0)) >= countMerge(m.requests) AND max(ifNull(pc.cache_expected, 0)) > 0 AND max(ifNull(pc.cache_{axis}_n, 0)) = max(ifNull(pc.cache_expected, 0))"
        )
    };
    let expression = match name {
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
    };
    format!("toInt64({expression}) AS v_{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::measurement_coverage::Coverage;

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
        for coverage in [
            Coverage::default(),
            Coverage {
                historical_units: true,
            },
        ] {
            let sql = source_with_coverage(
                "hour >= toDateTime('2026-09-01')",
                " AND user_id = 7",
                coverage,
            );
            assert!(sql.starts_with("(SELECT "), "{}", &sql[..40]);
            assert!(balanced(&sql));
            // 近百个列表达式引用 d 与测量子源：任何一个退回 CTE 或括号子查询都会让规划耗时数倍。
            assert!(!sql.contains("WITH d AS"), "不得使用 CTE");
            // 本层的 join：每个测量子源（`ON m.hour=…`）都必须是 view(...)。
            // 子源内部（别的模块生成的 e / a / r 等）的普通 join 不在此列。
            assert_eq!(
                sql.matches(" LEFT JOIN view(").count(),
                sql.matches(" ON m.hour=").count(),
                "测量子源必须以 view(...) 引用"
            );
            // 只读明细聚合，d 只出现一次：旧立方体剩余那一支会把 d 再分析一遍。
            assert_eq!(sql.matches("FROM view(SELECT ").count(), 1);
            assert!(!sql.contains("mv_cube_hour") && !sql.contains("UNION ALL SELECT l."));
        }
    }
}
