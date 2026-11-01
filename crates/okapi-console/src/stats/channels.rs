//! 渠道健康：渠道列表的用量与成功率、单渠道用量、渠道时间线。
use super::super::query::Query;
use super::{WindowQuery, ch_i64, ch_or_disabled, page_summary, rate_bp};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// GET /admin/stats/channels：渠道健康（错误率 / TTFT 分位 / 切换 / 粘性命中）。
/// 数据源 mv_channel_5min——此前该视图的 errors 与分位数列无任何查询出口。
pub async fn channels(
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
        "SELECT channel_id, \
                countMerge(requests) AS requests, \
                sumMerge(errors) AS errors, \
                sumMerge(amount) AS amount_micro, \
                sumMerge(upstream_cost) AS upstream_cost_micro, \
                sumMerge(failovers) AS failovers, \
                countIfMerge(sticky_resp_hits) AS sticky_resp_hits, \
                countIfMerge(sticky_sess_hits) AS sticky_sess_hits, \
                sumMerge(completion_tokens_sum) AS completion_tokens, count() OVER () AS total_items \
         FROM mv_channel_5min WHERE ts5 >= fromUnixTimestamp({since}) \
         GROUP BY channel_id ORDER BY requests DESC, channel_id ASC LIMIT {limit} OFFSET {offset}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    let summary = page_summary(ch, &rows, &format!(
        "SELECT uniqExact(channel_id) AS total_items FROM mv_channel_5min WHERE ts5 >= fromUnixTimestamp({since})"
    )).await?;

    // 渠道名补齐（PG 点查；榜单 ≤100 行，与 leaderboard 同法）
    let ids: Vec<i64> = rows.iter().map(|r| ch_i64(r, "channel_id")).collect();
    let named = sqlx::query!(
        r#"SELECT id, name, provider FROM channels WHERE id = ANY($1)"#,
        &ids
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let meta: HashMap<i64, (String, String)> = named
        .into_iter()
        .map(|r| (r.id, (r.name, r.provider)))
        .collect();

    let mut data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id = ch_i64(r, "channel_id");
            let requests = ch_i64(r, "requests");
            let errors = ch_i64(r, "errors");
            let (name, provider) = meta.get(&id).cloned().unwrap_or_default();
            json!({
                "channel_id": id,
                "name": name,
                "provider": provider,
                "requests": requests,
                "errors": errors,
                // 错误率与生成速度以基点/整数表达，避免前端拿到浮点再二次换算
                "error_rate_bp": rate_bp(errors, requests),
                "failovers": ch_i64(r, "failovers"),
                "sticky_hits": ch_i64(r, "sticky_resp_hits") + ch_i64(r, "sticky_sess_hits"),
                "sticky_rate_bp": rate_bp(
                    ch_i64(r, "sticky_resp_hits") + ch_i64(r, "sticky_sess_hits"),
                    requests,
                ),
                "amount_micro": ch_i64(r, "amount_micro"),
                "upstream_cost_micro": ch_i64(r, "upstream_cost_micro"),
            })
        })
        .collect();
    super::super::ttft::enrich(ch, &mut data, super::super::ttft::Scope::Channel, since).await?;
    super::super::usage_sources::enrich(
        ch,
        "channel_id",
        "mv_channel_5min",
        &format!("ts5 >= fromUnixTimestamp({since})"),
        &mut data,
    )
    .await?;
    Ok(Json(
        json!({ "days": days, "limit": limit, "offset": offset, "total_items": ch_i64(&summary, "total_items"), "data": data }),
    ))
}

/// GET /admin/stats/channel-usage：每条渠道窗口内的 token 与金额（渠道列表行内展示）。
///
/// 健康榜单（`/admin/stats/channels`）按请求数分页、来自 `mv_channel_5min`，没有输入 token；
/// 这里一次给全部有流量的渠道，列表按 id 对上即可。`mv_cube_hour` 小时粒度，窗口起点取整到小时。
pub async fn channel_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days.unwrap_or(30).clamp(1, 90);
    let since = chrono::Utc::now().timestamp() - i64::from(days) * 86_400;
    let rows = ch
        .query_json_each_row(&format!(
            "SELECT channel_id, \
                    countMerge(requests) AS requests, \
                    sumMerge(prompt_tokens) AS prompt_tokens, \
                    sumMerge(cached_tokens) AS cached_tokens, \
                    sumMerge(completion_tokens) AS completion_tokens, \
                    sumMerge(amount) AS amount_micro, \
                    sumMerge(upstream_cost) AS upstream_cost_micro \
             FROM mv_cube_hour WHERE hour >= toStartOfHour(fromUnixTimestamp({since})) \
             GROUP BY channel_id ORDER BY channel_id LIMIT 10000"
        ))
        .await
        .map_err(AppError::from)?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "channel_id": ch_i64(r, "channel_id"),
                "requests": ch_i64(r, "requests"),
                "prompt_tokens": ch_i64(r, "prompt_tokens"),
                "cached_tokens": ch_i64(r, "cached_tokens"),
                "completion_tokens": ch_i64(r, "completion_tokens"),
                "amount_micro": ch_i64(r, "amount_micro"),
                "upstream_cost_micro": ch_i64(r, "upstream_cost_micro"),
            })
        })
        .collect();
    Ok(Json(json!({ "days": days, "data": data })))
}

#[derive(Deserialize)]
pub struct TimelineQuery {
    /// 回看小时数（1–168，缺省 24）。
    #[serde(default)]
    pub hours: Option<u32>,
}

/// GET /admin/stats/channels/{id}/timeline：单条渠道的 5 分钟粒度健康时间线。
///
/// 渠道列表的"近 24h 错误率 25%"只回答**有多糟**，回答不了**从几点开始糟、
/// 现在还在糟吗**——而这两问决定"等它自愈"还是"现在就切"。mv_channel_5min
/// 本就按 5 分钟聚合，24h = 288 个点，直接出图（Sub2API 账号统计弹窗 /
/// 老 ok-api ChannelHealthGrid 都有单渠道时间轴，我们此前只有整窗汇总一行）。
pub async fn channel_timeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Query(q): Query<TimelineQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let hours = q.hours.unwrap_or(24).clamp(1, 168);
    let id = id.max(0);

    // 别名不与 MV 原始列同名（ts5 → bucket、errors → errs），GROUP/ORDER 用原始列
    let since = chrono::Utc::now().timestamp() - i64::from(hours) * 3_600;
    let sql = format!(
        "SELECT toString(ts5) AS bucket, \
                countMerge(requests) AS reqs, \
                sumMerge(errors) AS errs, \
                sumMerge(failovers) AS fo, \
                sumMerge(completion_tokens_sum) AS completion_tokens \
         FROM mv_channel_5min \
         WHERE channel_id = {id} AND ts5 >= fromUnixTimestamp({since}) \
         GROUP BY ts5 ORDER BY ts5"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    let mut total_reqs = 0_i64;
    let mut total_errs = 0_i64;
    let mut data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let reqs = ch_i64(r, "reqs");
            let errs = ch_i64(r, "errs");
            total_reqs = total_reqs.saturating_add(reqs);
            total_errs = total_errs.saturating_add(errs);
            json!({
                "bucket": r.get("bucket").and_then(Value::as_str).unwrap_or_default(),
                "requests": reqs,
                "errors": errs,
                "error_rate_bp": rate_bp(errs, reqs),
                "failovers": ch_i64(r, "fo"),
            })
        })
        .collect();
    super::super::ttft::enrich(
        ch,
        &mut data,
        super::super::ttft::Scope::Timeline(id),
        since,
    )
    .await?;
    super::super::usage_sources::enrich(
        ch,
        "ts5",
        "mv_channel_5min",
        &format!("channel_id = {id} AND ts5 >= fromUnixTimestamp({since})"),
        &mut data,
    )
    .await?;

    Ok(Json(json!({
        "channel_id": id,
        "hours": hours,
        "requests": total_reqs,
        "errors": total_errs,
        "error_rate_bp": rate_bp(total_errs, total_reqs),
        "data": data,
    })))
}
