//! 统一 14 维分析数据源：单表直读 `mv_analysis_5min`（零 7 路 JOIN），仅在窗口内存在
//! 历史语音字符校准记录时追加 1 路 `legacy_speech_units_v1` 左连接。
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
    ("latency_sum", "sumIfMerge"),
    ("latency_samples", "countIfMerge"),
    ("latency_observed", "countMerge"),
    ("latency_output", "sumIfMerge"),
    ("ttft_sum", "sumIfMerge"),
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

/// 把括号子查询改成 `view(...)` 表函数，供外层 / join 引用，避免 ClickHouse 24.8 分析器重复哈希子树。
pub(super) fn as_view(subquery: &str) -> String {
    if subquery.starts_with('(') {
        format!("view{subquery}")
    } else {
        subquery.to_owned()
    }
}

pub(super) fn join_on(left: &str, right: &str, keys: &str) -> String {
    keys.split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(|key| format!("{left}.{key}={right}.{key}"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// 将指标名映射到单宽表 `mv_analysis_5min` 的聚合表达式（表别名 `m`，避免与 SELECT 别名同名冲突）。
fn wide_metric_expr(name: &str) -> String {
    match name {
        "legacy_characters" | "legacy_character_n" => "toInt64(0)".to_owned(),
        "requests"
        | "output_rate_observed"
        | "unit_observed"
        | "latency_observed"
        | "ttft_observed"
        | "source_observed"
        | "cache_observed"
        | "ttl_observed"
        | "detail_observed" => "toInt64(countMerge(m.requests))".to_owned(),
        "financial_records" => "toInt64(countMerge(m.financial_records))".to_owned(),
        "prompt_tokens" | "source_prompt_total" => "toInt64(sumMerge(m.prompt_tokens))".to_owned(),
        "cached_tokens" | "source_cached_total" => "toInt64(sumMerge(m.cached_tokens))".to_owned(),
        "completion_tokens" | "source_completion_total" => {
            "toInt64(sumMerge(m.completion_tokens))".to_owned()
        }
        "reasoning_tokens" | "amount" | "discount" | "upstream_cost" | "errors"
        | "cache_writes" => format!("toInt64(sumMerge(m.{name}))"),
        "source_read_n" => "toInt64(countIfMerge(m.cache_read_n))".to_owned(),
        "output_rate_samples"
        | "output_rate_known"
        | "output_rate_tokens"
        | "unit_character_n"
        | "unit_token_n"
        | "latency_samples"
        | "ttft_samples"
        | "cache_write_n"
        | "cache_read_n" => format!("toInt64(countIfMerge(m.{name}))"),
        _ if name.ends_with("_n") => format!("toInt64(countIfMerge(m.{name}))"),
        _ => format!("toInt64(sumIfMerge(m.{name}))"),
    }
}

/// `window` 和 `scope` 是已校验的 SQL，字符串过滤均由调用方绑定。
///
/// 常态（无历史语音字符校准记录）下为单表直查 `mv_analysis_5min`、零 `LEFT JOIN`；
/// 仅在存在 `legacy_speech_units_v1` 记录时追加 1 路校准连接。
pub fn source_with_coverage(
    window: &str,
    scope: &str,
    coverage: super::measurement_coverage::Coverage,
) -> String {
    let predicate = format!("{window}{scope}");
    let detail_keys = format!("{KEYS}, {DIMS}");
    let merged = METRICS
        .iter()
        .map(|(name, _)| format!("{} AS {name}", wide_metric_expr(name)))
        .collect::<Vec<_>>()
        .join(", ");
    let base = format!(
        "(WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day \
          SELECT {detail_keys}, {merged}, \
                 toInt64(sumMerge(m.cache_writes)) AS write_tokens, \
                 toInt64(countIfMerge(m.cache_write_n)) AS write_samples, \
                 toInt64(countIfMerge(m.cache_read_n)) AS read_samples, \
                 toInt64(countIfMerge(m.cost_known)) AS cost_samples, \
                 sumMerge(m.known_amount) AS covered_amount, \
                 sumMerge(m.known_cost) AS covered_cost, \
                 maxMerge(m.last_event) AS event_at, \
                 maxMerge(m.last_ingested) AS ingested_at \
          FROM mv_analysis_5min AS m WHERE {predicate} GROUP BY {detail_keys})"
    );
    if !coverage.historical_units {
        return base;
    }
    let correction = super::input_units::correction_source(&detail_keys, &predicate);
    let keys_sel = detail_keys
        .split(", ")
        .map(|key| format!("m.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let chars = "toInt64(ifNull(c.legacy_characters, 0))";
    let count = "toInt64(ifNull(c.legacy_character_n, 0))";
    let calibrated = METRICS
        .iter()
        .map(|(name, _)| match *name {
            "legacy_characters" => format!("{chars} AS legacy_characters"),
            "legacy_character_n" => format!("{count} AS legacy_character_n"),
            "prompt_tokens" => format!(
                "accurateCast(m.prompt_tokens,'Int64')-{chars}+toInt64(throwIf({chars}>accurateCast(m.prompt_tokens,'Int64'))) AS prompt_tokens"
            ),
            "source_prompt_total" => format!("m.source_prompt_total-{chars} AS source_prompt_total"),
            "unit_observed" => format!(
                "greatest(m.unit_observed,m.unit_character_n+m.unit_token_n+{count})+toInt64(throwIf({count}+m.unit_character_n+m.unit_token_n>m.requests)) AS unit_observed"
            ),
            "unit_characters" => format!("m.unit_characters+{chars} AS unit_characters"),
            "unit_character_n" => format!("m.unit_character_n+{count} AS unit_character_n"),
            "output_rate_known" => format!("m.output_rate_known+{count} AS output_rate_known"),
            "output_rate_observed" => format!(
                "greatest(m.output_rate_observed,m.output_rate_known+{count})+toInt64(throwIf(m.output_rate_known+{count}>m.requests)) AS output_rate_observed"
            ),
            _ => format!("m.{name} AS {name}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(SELECT {keys_sel}, {calibrated}, \
                 m.write_tokens AS write_tokens, m.write_samples AS write_samples, m.read_samples AS read_samples, \
                 m.cost_samples AS cost_samples, m.covered_amount AS covered_amount, m.covered_cost AS covered_cost, \
                 m.event_at AS event_at, m.ingested_at AS ingested_at \
          FROM {} m LEFT JOIN {} c ON {})",
        as_view(&base),
        as_view(&correction),
        join_on("m", "c", &detail_keys),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement_coverage::Coverage;

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
        assert_eq!(as_view("mv_analysis_5min"), "mv_analysis_5min");
    }

    #[test]
    fn normal_analytics_source_uses_single_wide_mv_without_joins() {
        let sql = source_with_coverage(
            "hour >= toDateTime('2026-09-01')",
            " AND user_id = 7",
            Coverage::default(),
        );
        assert!(sql.starts_with("(WITH "), "{}", &sql[..40]);
        assert!(balanced(&sql));
        assert!(
            sql.len() < 8_192,
            "single-MV SQL should stay small: {}",
            sql.len()
        );
        assert_eq!(
            sql.matches(" JOIN ").count(),
            0,
            "no LEFT JOIN without legacy units"
        );
        assert!(sql.contains("FROM mv_analysis_5min AS m WHERE "));
        assert!(!sql.contains("mv_cube_hour"));
        assert!(!sql.contains("mv_analysis_hour"));
    }

    #[test]
    fn historical_units_add_only_one_view_isolated_calibration_join() {
        let sql = source_with_coverage(
            "hour >= toDateTime('2026-09-01')",
            " AND user_id = 7",
            Coverage {
                historical_units: true,
            },
        );
        assert!(balanced(&sql));
        assert_eq!(sql.matches(" LEFT JOIN view(").count(), 1);
        assert_eq!(sql.matches(" LEFT JOIN ").count(), 1);
        assert!(sql.contains("legacy_speech_units_v1 FINAL"));
    }
}
