//! 模型：分位延迟与吞吐、按模型的趋势。
use super::super::query::Query;
use super::{WindowQuery, ch_i64, ch_or_disabled, page_summary};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use serde_json::{Value, json};
use std::collections::HashMap;

/// GET /admin/stats/models：模型时延分位与生成速度（mv_model_hour，此前完全无出口）。
pub async fn models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let limit = q.limit();
    let offset = q.offset();

    let since = chrono::Utc::now().timestamp() - i64::from(days) * 86_400;
    let sql = format!(
        "SELECT model, \
                countMerge(requests) AS requests, \
                sumMerge(tokens) AS tokens, \
                sumMerge(amount) AS amount_micro, \
                sumMerge(completion_tokens_sum) AS completion_tokens, count() OVER () AS total_items \
         FROM mv_model_hour WHERE hour >= fromUnixTimestamp({since}) \
         GROUP BY model ORDER BY requests DESC, model ASC LIMIT {limit} OFFSET {offset}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    let summary = page_summary(ch, &rows, &format!(
        "SELECT uniqExact(model) AS total_items FROM mv_model_hour WHERE hour >= fromUnixTimestamp({since})"
    )).await?;

    let mut data: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "model": r.get("model").and_then(Value::as_str).unwrap_or_default(),
                "requests": ch_i64(r, "requests"),
                "tokens": ch_i64(r, "tokens"),
                "amount_micro": ch_i64(r, "amount_micro"),
            })
        })
        .collect();
    super::super::ttft::enrich(ch, &mut data, super::super::ttft::Scope::Model, since).await?;
    super::super::usage_sources::enrich(
        ch,
        "model",
        "mv_model_hour",
        &format!("hour >= fromUnixTimestamp({since})"),
        &mut data,
    )
    .await?;
    Ok(Json(
        json!({ "days": days, "limit": limit, "offset": offset, "total_items": ch_i64(&summary, "total_items"), "data": data }),
    ))
}

/// 折叠桶名：Top N 之外的模型归并到这里（前端渲染为"其他"并配灰色）。
/// 名字带双下划线防与真实模型名撞车。
pub(super) const OTHER_BUCKET: &str = "__other";

/// GET /admin/stats/model-trend：按模型堆叠的消耗趋势
/// （new-api 数据看板的招牌视图；老 ok-api GetModelConsumptionTrend 同源）。
///
/// 与 `/admin/stats/models` 的分工：那边是模型**表现**（时延分位/吞吐，表格），
/// 这边是模型**花销随时间**（堆叠图数据源）——"钱花在哪个模型、占比怎么变"
/// 此前只有全站总量曲线，没有按模型的拆分出口。
///
/// Top N 由窗口总消耗决定，其余折叠进 `__other`：站点动辄几十上百个模型，
/// 全画进堆叠图只会得到一坨图例。折叠在 Rust 侧完成——模型名不进 SQL
/// （虽然它来自 CH 自身，但拼进 IN 列表就得处理引号转义，绑定参数又不支持数组）。
pub async fn model_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    // 堆叠图例的可读上限远低于表格，缺省 8、封顶 20
    let limit = q.limit.unwrap_or(8).clamp(1, 20) as usize;

    // 单日窗口按小时出桶（看"今天几点开始烧钱"），多日按天
    let (bucket_expr, granularity) = if days <= 1 {
        ("toString(toStartOfHour(hour))", "hour")
    } else {
        ("toString(toDate(hour))", "day")
    };
    let predicate = format!("hour >= now() - INTERVAL {days} DAY");
    let sql = format!(
        "SELECT {bucket_expr} AS bucket, model, \
                sumMerge(amount) AS spend, countMerge(requests) AS reqs \
         FROM mv_model_hour WHERE {predicate} \
         GROUP BY bucket, model ORDER BY bucket"
    );
    let mut rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    super::super::token_details::enrich_trend(ch, bucket_expr, &predicate, &mut rows).await?;
    let (series, data) = fold_model_trend(&rows, limit);

    Ok(Json(
        json!({ "days": days, "granularity": granularity, "models": series, "data": data }),
    ))
}

pub(super) fn fold_model_trend(rows: &[Value], limit: usize) -> (Vec<String>, Vec<Value>) {
    // 窗口总消耗排序取 Top N（占比小的模型进"其他"）
    let mut totals: HashMap<String, i64> = HashMap::new();
    for r in rows {
        let model = r.get("model").and_then(Value::as_str).unwrap_or_default();
        *totals.entry(model.to_owned()).or_default() += ch_i64(r, "spend");
    }
    let mut ranked: Vec<(String, i64)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top: Vec<String> = ranked.iter().take(limit).map(|(m, _)| m.clone()).collect();
    let has_other = ranked.len() > top.len();

    // bucket → (model → {spend, requests})，保持 SQL 的时间升序
    let mut order: Vec<String> = Vec::new();
    let mut folded: HashMap<String, HashMap<String, Value>> = HashMap::new();
    for r in rows {
        let bucket = r.get("bucket").and_then(Value::as_str).unwrap_or_default();
        let model = r.get("model").and_then(Value::as_str).unwrap_or_default();
        let slot = if top.iter().any(|m| m == model) {
            model
        } else {
            OTHER_BUCKET
        };
        if !folded.contains_key(bucket) {
            order.push(bucket.to_owned());
        }
        let cell = folded
            .entry(bucket.to_owned())
            .or_default()
            .entry(slot.to_owned())
            .or_default();
        if !cell.is_object() {
            *cell = json!({});
        }
        cell["spend"] = json!(ch_i64(cell, "spend").saturating_add(ch_i64(r, "spend")));
        cell["reqs"] = json!(ch_i64(cell, "reqs").saturating_add(ch_i64(r, "reqs")));
        super::super::token_details::accumulate(cell, r);
    }

    let mut series: Vec<String> = top;
    if has_other {
        series.push(OTHER_BUCKET.to_owned());
    }
    let data: Vec<Value> = order
        .iter()
        .map(|bucket| {
            let cells = &folded[bucket];
            let values: serde_json::Map<String, Value> = series
                .iter()
                .filter_map(|m| {
                    cells.get(m).map(|cell| {
                        let requests = ch_i64(cell, "reqs");
                        let mut metrics = super::super::token_details::metrics(cell, requests);
                        metrics.insert("amount_micro".into(), json!(ch_i64(cell, "spend")));
                        metrics.insert("requests".into(), json!(requests));
                        (m.clone(), Value::Object(metrics))
                    })
                })
                .collect();
            json!({ "bucket": bucket, "values": values })
        })
        .collect();

    (series, data)
}
