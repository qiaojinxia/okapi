//! 错误分布、客户端分布与单次请求的诊断。
use super::super::query::Query;
use super::{WindowQuery, ch_i64, ch_or_disabled, page_summary, rate_bp};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use serde_json::{Value, json};
use std::collections::HashMap;

/// GET /admin/stats/errors：错误码分布（mv_error_hour）。
///
/// 与 overview 的 `error_rate_bp` 互补：那个数字回答"坏得多不多"，
/// 这里回答"坏在哪"——按错误码聚合并带上出问题最多的渠道，
/// 使"降级 / 加容量 / 换供应商"成为可判断的下一步而不是猜。
pub async fn errors(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let limit = q.limit();
    let offset = q.offset();

    // 子查询列名（errs/ustatus）刻意与外层别名错开：ClickHouse 会把
    // `argMax(channel_id, errors)` 里的 errors 解析成外层 `sum(...) AS errors`
    // 自身，报"聚合套聚合"。
    let sql = format!(
        "SELECT error_code, sum(errs) AS errors, \
                argMax(channel_id, errs) AS top_channel_id, \
                argMax(model, errs) AS top_model, \
                max(ustatus) AS upstream_status, \
                sum(sum(errs)) OVER () AS total_errors, count() OVER () AS total_items \
         FROM ( \
             SELECT error_code, channel_id, model, \
                    countMerge(errors) AS errs, \
                    maxMerge(upstream_status) AS ustatus \
             FROM mv_error_hour WHERE hour >= now() - INTERVAL {days} DAY \
             GROUP BY error_code, channel_id, model \
         ) GROUP BY error_code ORDER BY errors DESC, error_code ASC LIMIT {limit} OFFSET {offset}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    let summary = page_summary(ch, &rows, &format!(
        "SELECT uniqExact(error_code) AS total_items, countMerge(errors) AS total_errors FROM mv_error_hour WHERE hour >= now() - INTERVAL {days} DAY"
    )).await?;

    let ids: Vec<i64> = rows.iter().map(|r| ch_i64(r, "top_channel_id")).collect();
    let channel_names: HashMap<i64, String> =
        sqlx::query!(r#"SELECT id, name FROM channels WHERE id = ANY($1)"#, &ids)
            .fetch_all(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .into_iter()
            .map(|r| (r.id, r.name))
            .collect();

    let total = ch_i64(&summary, "total_errors");
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let errors = ch_i64(r, "errors");
            let channel_id = ch_i64(r, "top_channel_id");
            json!({
                "error_code": r.get("error_code").and_then(Value::as_str).unwrap_or_default(),
                "errors": errors,
                "share_bp": rate_bp(errors, total),
                "upstream_status": ch_i64(r, "upstream_status"),
                "top_channel_id": channel_id,
                "top_channel_name": channel_names.get(&channel_id).cloned().unwrap_or_default(),
                "top_model": r.get("top_model").and_then(Value::as_str).unwrap_or_default(),
            })
        })
        .collect();
    Ok(Json(
        json!({ "days": days, "limit": limit, "offset": offset, "total_items": ch_i64(&summary, "total_items"), "total": total, "data": data }),
    ))
}

/// GET /admin/stats/clients：客户端类型分布（#5277，mv_client_day）。
///
/// 回答"流量从哪些工具来"——Claude Code / Codex / Cursor 这类编码智能体的
/// 占比决定站长该优先保障哪些协议面（anthropic 方向 vs openai 方向）与
/// prompt cache 透传的重要性（编码智能体的账单大头是缓存命中）。
/// `users` 是去重用户数：请求数说"谁在刷"，用户数说"谁在用"。
pub async fn clients(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let calendar =
        super::super::usage_details::CalendarWindow::read(ch, q.days(), None, None).await?;
    let days = calendar.days();
    let range = calendar.day_filter();
    let limit = q.limit();
    let offset = q.offset();

    let correction = super::super::input_units::correction_source("client_type", &range);
    let tokens = super::super::input_units::corrected_sql(
        "sumMerge(tokens)",
        "max(ifNull(c.legacy_characters,0))",
    );
    let sql = format!(
        "SELECT client_type, countMerge(requests) AS reqs, {tokens} AS toks, \
                sumMerge(amount) AS spend, sumMerge(errors) AS errs, uniqMerge(users) AS uniq_users, \
                sum(countMerge(requests)) OVER () AS total_requests, count() OVER () AS total_items \
         FROM mv_client_day LEFT JOIN {correction} c USING (client_type) WHERE {range} \
         GROUP BY client_type ORDER BY reqs DESC, client_type ASC LIMIT {limit} OFFSET {offset}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    let summary = page_summary(ch, &rows, &format!(
        "SELECT uniqExact(client_type) AS total_items, countMerge(requests) AS total_requests FROM mv_client_day WHERE {range}"
    )).await?;

    let total_requests = ch_i64(&summary, "total_requests");
    let total_clients = ch_i64(&summary, "total_items");
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let requests = ch_i64(r, "reqs");
            let errors = ch_i64(r, "errs");
            json!({
                // 空串 = UA 未命中任何规则；前端渲染为"未识别"而非空格
                "client_type": r.get("client_type").and_then(Value::as_str).unwrap_or_default(),
                "requests": requests,
                "share_bp": rate_bp(requests, total_requests),
                "tokens": ch_i64(r, "toks"),
                "amount_micro": ch_i64(r, "spend"),
                "errors": errors,
                "error_rate_bp": rate_bp(errors, requests),
                "users": ch_i64(r, "uniq_users"),
            })
        })
        .collect();
    Ok(Json(
        json!({ "days": days, "window": calendar.json(), "limit": limit, "offset": offset, "total_items": total_clients, "total_clients": total_clients, "total_requests": total_requests, "data": data }),
    ))
}

/// GET /admin/diagnose：全链路健康——PG/Redis/CH/NATS 可达、outbox 积压、DLQ 深度、
/// 冷却 key 数、PriceBook epoch。此前只有 MCP 出口（§7.2 诊断组），站长后台
/// 反而看不到；落地页"需要注意"面板据此报组件不可达与积压。
/// 权限沿用 billing.read：其中的 DLQ/outbox 都是账务链路状态。
pub async fn diagnose(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    Ok(Json(super::super::mcp::diagnose(&state).await?))
}
