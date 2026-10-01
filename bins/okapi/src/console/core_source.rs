//! 只要核心指标（请求、Token、金额、错误）时用的精简数据源。
//!
//! `analysis_source::source_with_coverage` 为了还原每个测量口径（用量来源、Token 明细、
//! 延迟、TTFT、缓存……）会拼出 ~100KB、约 300 个子查询的 SQL。ClickHouse 对它的
//! 分析与规划就要 ~5s 的单核 CPU，而实际只读几千行——耗时与数据量无关。
//! 首页排行、趋势图和各类"全量分母"只读下面九列，这些列在完整源里都是直接取自 MV 的
//! `*Merge`（不经任何测量子源 join），因此可以用同样的 `d / l / c` 结构只投影这九列，
//! 结果与完整源逐值相同（见 `console_analytics` 的对照用例），SQL 降到 ~3KB。
//!
//! 与完整源的差别只有两处，都不改变这九列的和：
//! - `d` 只按 `KEYS` 分组，不再带明细维度——明细维度只是同一 `KEYS` 下的细分，
//!   `c` 本来就要把 `d` 按 `KEYS` 汇总；调用方带明细过滤时必须回退完整源。
//! - 不 join 任何测量子源。`prompt_tokens` 只有在存在历史字符口径（`historical_units`）
//!   时才需要 `td` 校正，此时同样必须回退完整源。

const KEYS: &str = "hour, user_id, api_key_id, group_code, model, channel_id";

/// (列名, 合并函数)。与 `analysis_source::METRICS` 中同名项完全一致，单测守住。
const COLUMNS: [(&str, &str); 10] = [
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
];

/// 外层聚合；别名与完整源的 `analytics::AGG` 前九项一致。
pub(super) const AGG: &str = "sum(requests) AS reqs, sum(prompt_tokens) AS prompt, \
    sum(cached_tokens) AS cached, sum(completion_tokens) AS completion, \
    sum(reasoning_tokens) AS reasoning, sum(amount) AS spend, sum(discount) AS saved, \
    sum(upstream_cost) AS cost, sum(errors) AS errs";

/// `window` 与 `scope` 是调用方已校验的 SQL（只含主键维度整数条件），与完整源同口径。
pub(super) fn source(window: &str, scope: &str) -> String {
    let predicate = format!("{window}{scope}");
    let projection = KEYS
        .split(", ")
        .map(|key| format!("m.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let group = KEYS
        .split(", ")
        .map(|key| format!("m.{key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let legacy_keys = KEYS
        .split(", ")
        .map(|key| format!("l.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let merged = COLUMNS
        .iter()
        .map(|(name, func)| format!("toInt64({func}(m.{name})) AS v_{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sums = COLUMNS
        .iter()
        .map(|(name, _)| format!("sum(v_{name}) AS c_{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let detailed = COLUMNS
        .iter()
        .map(|(name, _)| format!("v_{name} AS {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let remainder = COLUMNS
        .iter()
        .map(|(name, _)| format!("l.v_{name} - ifNull(c.c_{name}, 0) AS {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    // 明细聚合已覆盖的粒度以明细为准；旧立方体比明细多出的请求数才作为剩余部分补上。
    format!(
        "(WITH \
        d AS (SELECT {projection}, {merged} FROM (SELECT * FROM mv_analysis_hour WHERE {predicate}) m GROUP BY {group}), \
        l AS (SELECT {projection}, {merged} FROM (SELECT * FROM mv_cube_hour WHERE {predicate}) m GROUP BY {group}), \
        c AS (SELECT {KEYS}, {sums} FROM d GROUP BY {KEYS}) \
        SELECT {KEYS}, {detailed} FROM d \
        UNION ALL \
        SELECT {legacy_keys}, {remainder} FROM l LEFT JOIN c USING ({KEYS}) WHERE l.v_financial_records > ifNull(c.c_financial_records, 0))"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_match_the_full_source_definitions() {
        for (name, func) in COLUMNS {
            assert!(
                super::super::analysis_source::METRICS.contains(&(name, func)),
                "{name}/{func} 必须与完整源 METRICS 保持一致"
            );
        }
    }

    /// 精简源成立的前提：这九列在完整源里都是直接取自 MV 的 `*Merge`，不经任何测量子源 join。
    /// 一旦哪列被加进某个测量子源的 FIELDS（或带 ttft_ / latency_ 前缀），完整源会改走 join，
    /// 而这里仍是纯 `*Merge`——两条路径悄悄分叉，且 ClickHouse 对照用例在没配 CH 的机器上会跳过。
    #[test]
    fn no_core_column_goes_through_a_measurement_source_in_the_full_builder() {
        use super::super::{cache_usage, input_units, output_rate, token_details, usage_sources};
        for (name, _) in COLUMNS {
            for (owner, fields) in [
                ("token_details", token_details::FIELDS.as_slice()),
                ("usage_sources", usage_sources::FIELDS.as_slice()),
                ("cache_usage", cache_usage::FIELDS.as_slice()),
                ("output_rate", output_rate::FIELDS.as_slice()),
                ("input_units", input_units::FIELDS.as_slice()),
            ] {
                assert!(!fields.contains(&name), "{name} 出现在 {owner}::FIELDS");
            }
            assert!(
                !name.starts_with("ttft_") && !name.starts_with("latency_"),
                "{name}"
            );
        }
    }

    #[test]
    fn aggregate_aliases_match_the_full_aggregate() {
        for (name, alias) in [
            ("requests", "reqs"),
            ("prompt_tokens", "prompt"),
            ("cached_tokens", "cached"),
            ("completion_tokens", "completion"),
            ("reasoning_tokens", "reasoning"),
            ("amount", "spend"),
            ("discount", "saved"),
            ("upstream_cost", "cost"),
            ("errors", "errs"),
        ] {
            assert!(AGG.contains(&format!("sum({name}) AS {alias}")), "{name}");
        }
    }

    #[test]
    fn source_has_no_measurement_joins_and_keeps_the_legacy_remainder() {
        let sql = source("hour >= toDateTime('2026-09-01')", " AND user_id = 7");
        assert!(sql.len() < 6_000, "精简源应保持在几 KB：{}", sql.len());
        assert_eq!(sql.matches(" LEFT JOIN ").count(), 1);
        assert!(sql.contains("FROM l LEFT JOIN c USING"));
        assert!(sql.contains("WHERE l.v_financial_records > ifNull(c.c_financial_records, 0)"));
        assert!(
            sql.contains("mv_analysis_hour WHERE hour >= toDateTime('2026-09-01') AND user_id = 7")
        );
        assert!(
            sql.contains("mv_cube_hour WHERE hour >= toDateTime('2026-09-01') AND user_id = 7")
        );
        for forbidden in [
            "mv_token_details",
            "mv_usage_sources",
            "latency",
            "ttft",
            "cache_",
        ] {
            assert!(!sql.contains(forbidden), "{forbidden}");
        }
    }
}
