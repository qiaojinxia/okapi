//! 趋势：按时间桶的度量序列与堆叠维度。
use super::super::query::Query;
use super::super::stats::ch_i64;
use super::{
    CubeQuery, FLOW_OTHER, Names, Scope, accumulate_core_metrics, accumulate_original,
    aggregate_metrics, ch_str, describe_scope, pack_core_metrics, pack_metrics, resolve_names,
    row_key, sum_core_rows, sum_metric_rows,
};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use serde_json::{Value, json};
use std::collections::HashMap;

/// GET /admin/stats/trend：过滤后的时间趋势 + 当前 / 上一窗口汇总。
///
/// 单日 / 两日窗口按小时出桶（"今天几点开始烧钱"），更长按天。`total` 与
/// `previous` 是同长度的两段，前端据此给每张 KPI 卡标环比箭头。
pub async fn trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(mut q): Query<CubeQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    q.stack_column()?;
    q.cached &= super::super::stats_cache::allowed(&headers);
    q.prepare(&state).await?;
    let days = q.days();
    let scope = q.scope();
    let params = scope.borrow();

    let mut window = q.window_meta.clone();
    window["freshness"] = super::super::analysis_freshness::read(&state).await?;

    let (bucket_expr, granularity) = if q.granularity.as_deref() == Some("hour") {
        ("toString(hour)", "hour")
    } else {
        ("toString(toDate(hour))", "day")
    };
    // 核心指标走精简源：只读 amount / requests / tokens 的调用方（首页趋势图）毫秒级返回，
    // 不必为 ~300 个测量子查询付 ~5s 的规划时间。堆叠与带明细过滤时回退完整源。
    if q.core_requested()? && q.stack_column()?.is_none() && q.core_source_ok(None, false) {
        return core_trend(&state, &q, &scope, bucket_expr, granularity, window).await;
    }

    let agg = aggregate_metrics();
    let current_source = q.source(false);
    let series_sql = format!(
        "SELECT {bucket_expr} AS bucket, {agg} FROM {current_source} \
         WHERE {}{} GROUP BY bucket ORDER BY bucket",
        q.window(false),
        scope.clause
    );
    let pack_one = |rows: &[Value]| {
        rows.first()
            .map_or_else(|| json!({}), |r| Value::Object(pack_metrics(r)))
    };

    // 堆叠：按第二维度拆开每个桶（"钱花在哪个模型、占比怎么变"），Top N 之外折进 __other
    if let Some(stack_col) = q.stack_column()? {
        let limit = q.limit.unwrap_or(8).clamp(1, 20) as usize;
        let stacked_sql = format!(
            "SELECT {bucket_expr} AS bucket, {stack_col} AS k, {agg} \
             FROM {current_source} WHERE {}{} GROUP BY bucket, k ORDER BY bucket",
            q.window(false),
            scope.clause
        );
        let (rows, previous) = tokio::try_join!(
            super::super::stats_cache::query(&state, &stacked_sql, &params, q.cached),
            q.previous_totals(&state, &agg, &scope),
        )?;
        let total = vec![sum_metric_rows(&rows)];
        let (series, data) = fold_stacked(
            &rows,
            limit,
            match q.metric.as_deref() {
                Some("requests" | "latency" | "ttft" | "error_rate" | "cache" | "throughput") => {
                    "reqs"
                }
                Some("tokens") => "tokens",
                _ => "spend",
            },
        );
        let labels = stack_labels(&state, q.stack.as_deref().unwrap_or_default(), &series).await?;
        return Ok(Json(json!({
            "days": days,
            "granularity": granularity,
            "window": window,
            "scope": describe_scope(&state, &q).await?,
            "total": pack_one(&total),
            "fields": "all",
            "previous": pack_one(&previous),
            "stack": q.stack,
            "series": series.iter().map(|k| json!({ "key": k, "label": labels.get(k) })).collect::<Vec<_>>(),
            "data": data,
        })));
    }

    let (series, previous) = tokio::try_join!(
        super::super::stats_cache::query(&state, &series_sql, &params, q.cached),
        q.previous_totals(&state, &agg, &scope),
    )?;
    let total = vec![sum_metric_rows(&series)];
    let data: Vec<Value> = series
        .iter()
        .map(|r| {
            let mut m = pack_metrics(r);
            m.insert("bucket".into(), json!(ch_str(r, "bucket")));
            Value::Object(m)
        })
        .collect();

    Ok(Json(json!({
        "days": days,
        "granularity": granularity,
        "window": window,
        "scope": describe_scope(&state, &q).await?,
        "total": pack_one(&total),
        "fields": "all",
        "previous": pack_one(&previous),
        "data": data,
    })))
}

/// `fields=core` 的趋势：逐桶与总计只含核心指标，不查测量口径，也不查上一窗口。
pub(super) async fn core_trend(
    state: &AppState,
    q: &CubeQuery,
    scope: &Scope,
    bucket_expr: &str,
    granularity: &str,
    window: Value,
) -> Result<Json<Value>, AppError> {
    let sql = format!(
        "SELECT {bucket_expr} AS bucket, {} FROM {} WHERE {}{} GROUP BY bucket ORDER BY bucket",
        super::super::core_source::AGG,
        q.core_source(false),
        q.window(false),
        scope.clause
    );
    let series = super::super::stats_cache::query(state, &sql, &scope.borrow(), q.cached).await?;
    let data: Vec<Value> = series
        .iter()
        .map(|r| {
            let mut m = pack_core_metrics(r);
            m.insert("bucket".into(), json!(ch_str(r, "bucket")));
            Value::Object(m)
        })
        .collect();
    Ok(Json(json!({
        "days": q.days(),
        "granularity": granularity,
        "window": window,
        "scope": describe_scope(state, q).await?,
        "fields": "core",
        "total": pack_core_metrics(&sum_core_rows(&series)),
        "previous": {},
        "data": data,
    })))
}

/// 堆叠行（bucket × k）→ Top N 序列 + 逐桶数值；其余折进 `__other`。
/// 排名按窗口金额；模型名不进 SQL 省掉 IN 列表转义（与 model_trend 同法）。
pub(super) fn fold_stacked(rows: &[Value], limit: usize, rank: &str) -> (Vec<String>, Vec<Value>) {
    let mut totals: HashMap<String, i64> = HashMap::new();
    for r in rows {
        *totals.entry(row_key(r)).or_default() += if rank == "tokens" {
            ch_i64(r, "prompt").saturating_add(ch_i64(r, "completion"))
        } else {
            ch_i64(r, rank)
        };
    }
    let mut ranked: Vec<(String, i64)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top: Vec<String> = ranked.iter().take(limit).map(|(k, _)| k.clone()).collect();
    let has_other = ranked.len() > top.len();

    let mut order: Vec<String> = Vec::new();
    let mut folded: HashMap<String, HashMap<String, Value>> = HashMap::new();
    for r in rows {
        let bucket = ch_str(r, "bucket");
        let key = row_key(r);
        let slot = if top.contains(&key) {
            key
        } else {
            FLOW_OTHER.to_owned()
        };
        if !folded.contains_key(bucket) {
            order.push(bucket.to_owned());
        }
        let cell = folded
            .entry(bucket.to_owned())
            .or_default()
            .entry(slot)
            .or_default();
        if !cell.is_object() {
            *cell = json!({});
        }
        accumulate_original(cell, r);
        super::super::usage_sources::accumulate(cell, r);
        super::super::token_details::accumulate(cell, r);
        super::super::output_rate::accumulate(cell, r);
        accumulate_core_metrics(cell, r);
    }
    let mut series = top;
    if has_other {
        series.push(FLOW_OTHER.to_owned());
    }
    let data = order
        .iter()
        .map(|bucket| {
            let cells = &folded[bucket];
            let values: serde_json::Map<String, Value> = series
                .iter()
                .filter_map(|k| {
                    cells
                        .get(k)
                        .map(|c| (k.clone(), Value::Object(pack_metrics(c))))
                })
                .collect();
            json!({ "bucket": bucket, "values": values })
        })
        .collect();
    (series, data)
}

/// 堆叠序列的展示名：user / api_key / channel 从 PG 回填，其余键即名。
pub(super) async fn stack_labels(
    state: &AppState,
    stack: &str,
    keys: &[String],
) -> Result<HashMap<String, String>, AppError> {
    let ids: Vec<i64> = keys.iter().filter_map(|k| k.parse::<i64>().ok()).collect();
    let names = match stack {
        "user" => resolve_names(state, &ids, &[], &[], &[]).await?,
        "api_key" => resolve_names(state, &[], &ids, &[], &[]).await?,
        "channel" => resolve_names(state, &[], &[], &ids, &[]).await?,
        _ => Names::default(),
    };
    Ok(keys
        .iter()
        .filter_map(|k| {
            let id = k.parse::<i64>().ok()?;
            let label = match stack {
                "user" => names.users.get(&id).cloned(),
                "api_key" => names.keys.get(&id).map(|k| k.0.clone()),
                "channel" => names.channels.get(&id).map(|c| c.0.clone()),
                _ => None,
            }?;
            Some((k.clone(), label))
        })
        .collect())
}
/// 拆分维度 → 立方体键列。
/// 立方体主键里的维度列：精简源只含这些维度上的核心指标。
pub(super) fn primary_key_column(column: &str) -> bool {
    matches!(
        column,
        "model" | "channel_id" | "user_id" | "api_key_id" | "group_code"
    )
}
