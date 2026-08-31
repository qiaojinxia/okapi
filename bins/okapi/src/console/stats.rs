//! 运营看板查询（IMPLEMENTATION §10 指标字典）。
//!
//! 只读 ClickHouse 物化视图——重活在写入侧由 MV 增量完成，这里全是 `*Merge`
//! 单表聚合，不碰热路径、不落 PG 写。CH 不可用时按既有约定 501 `stats_disabled`。
//!
//! SQL 用 format! 拼装但**只插入 clamp 过的整数**（与 leaderboard 同一纪律），
//! 请求里的字符串一律不进 SQL。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

fn ch_or_disabled(state: &AppState) -> Result<&ChClient, AppError> {
    state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))
}

/// CH 的 JSONEachRow 把 64 位整数序列化为字符串，需两种形态都认。
fn ch_i64(row: &Value, key: &str) -> i64 {
    row.get(key).map_or(0, |v| {
        v.as_str()
            .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
            .unwrap_or(0)
    })
}

/// 分位数取整在 SQL 侧完成（`toUInt32(...[n])`），Rust 侧只见整数——
/// 避免为展示指标在后端引入浮点与有损转换。
fn quantile_cols(state_col: &str, prefix: &str) -> String {
    ["p50", "p95", "p99"]
        .iter()
        .enumerate()
        .map(|(idx, tag)| {
            format!(
                "toUInt32(quantilesMerge(0.5, 0.95, 0.99)({state_col})[{}]) AS {prefix}_{tag}_ms",
                idx + 1
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Deserialize)]
pub struct WindowQuery {
    /// 回看天数（1–90，缺省 7）。
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl WindowQuery {
    fn days(&self) -> u32 {
        self.days.unwrap_or(7).clamp(1, 90)
    }
    fn limit(&self) -> u32 {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

/// GET /admin/stats/channels：渠道健康（错误率 / TTFT 分位 / 切换 / 粘性命中）。
/// 数据源 mv_channel_5min——此前该视图的 errors 与分位数列无任何查询出口。
pub async fn channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let limit = q.limit();

    let ttft = quantile_cols("ttft_q", "ttft");
    let sql = format!(
        "SELECT channel_id, \
                countMerge(requests) AS requests, \
                sumMerge(errors) AS errors, \
                sumMerge(amount) AS amount_micro, \
                sumMerge(upstream_cost) AS upstream_cost_micro, \
                {ttft}, \
                sumMerge(failovers) AS failovers, \
                countIfMerge(sticky_resp_hits) AS sticky_resp_hits, \
                countIfMerge(sticky_sess_hits) AS sticky_sess_hits, \
                sumMerge(completion_tokens_sum) AS completion_tokens, \
                sumMerge(latency_sum) AS latency_ms_sum \
         FROM mv_channel_5min WHERE ts5 >= now() - INTERVAL {days} DAY \
         GROUP BY channel_id ORDER BY requests DESC LIMIT {limit}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

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

    let data: Vec<Value> = rows
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
                "ttft_p50_ms": ch_i64(r, "ttft_p50_ms"),
                "ttft_p95_ms": ch_i64(r, "ttft_p95_ms"),
                "ttft_p99_ms": ch_i64(r, "ttft_p99_ms"),
                "failovers": ch_i64(r, "failovers"),
                "sticky_hits": ch_i64(r, "sticky_resp_hits") + ch_i64(r, "sticky_sess_hits"),
                "sticky_rate_bp": rate_bp(
                    ch_i64(r, "sticky_resp_hits") + ch_i64(r, "sticky_sess_hits"),
                    requests,
                ),
                "tokens_per_1k_sec": tokens_per_1k_sec(r),
                "amount_micro": ch_i64(r, "amount_micro"),
                "upstream_cost_micro": ch_i64(r, "upstream_cost_micro"),
            })
        })
        .collect();
    Ok(Json(json!({ "days": days, "data": data })))
}

/// 占比 → 基点（万分之一；整数运算，分母 0 返 0）。
fn rate_bp(part: i64, total: i64) -> i64 {
    if total <= 0 {
        return 0;
    }
    part.saturating_mul(10_000) / total
}

/// token 加权生成速度（IMPLEMENTATION §10 / new-api #5029 口径）：
/// Σcompletion_tokens / Σlatency，放大 1000 倍后为「每千秒 token 数」的整数表达，
/// 前端除以 1000 得 tok/s——全程整数，不在计费无关路径引入浮点漂移。
fn tokens_per_1k_sec(row: &Value) -> i64 {
    let tokens = ch_i64(row, "completion_tokens");
    let ms = ch_i64(row, "latency_ms_sum");
    if ms <= 0 {
        return 0;
    }
    tokens.saturating_mul(1_000_000) / ms
}

/// GET /admin/stats/models：模型时延分位与生成速度（mv_model_hour，此前完全无出口）。
pub async fn models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let limit = q.limit();

    let ttft = quantile_cols("ttft_q", "ttft");
    let latency = quantile_cols("latency_q", "latency");
    let sql = format!(
        "SELECT model, \
                countMerge(requests) AS requests, \
                sumMerge(tokens) AS tokens, \
                sumMerge(amount) AS amount_micro, \
                {ttft}, {latency}, \
                sumMerge(completion_tokens_sum) AS completion_tokens, \
                sumMerge(latency_sum) AS latency_ms_sum \
         FROM mv_model_hour WHERE hour >= now() - INTERVAL {days} DAY \
         GROUP BY model ORDER BY requests DESC LIMIT {limit}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "model": r.get("model").and_then(Value::as_str).unwrap_or_default(),
                "requests": ch_i64(r, "requests"),
                "tokens": ch_i64(r, "tokens"),
                "amount_micro": ch_i64(r, "amount_micro"),
                "ttft_p50_ms": ch_i64(r, "ttft_p50_ms"),
                "ttft_p95_ms": ch_i64(r, "ttft_p95_ms"),
                "ttft_p99_ms": ch_i64(r, "ttft_p99_ms"),
                "latency_p50_ms": ch_i64(r, "latency_p50_ms"),
                "latency_p95_ms": ch_i64(r, "latency_p95_ms"),
                "latency_p99_ms": ch_i64(r, "latency_p99_ms"),
                "tokens_per_1k_sec": tokens_per_1k_sec(r),
            })
        })
        .collect();
    Ok(Json(json!({ "days": days, "data": data })))
}

/// GET /admin/stats/margin：经营口径逐日聚合（实收 / 标价 / 让利 / 毛利）。
/// 数据源 mv_user_day 的 original / discount / upstream_cost 三列，此前无查询出口。
///
/// **已知边界**：`upstream_cost` 目前全链路恒为 0——chsink `build_ch_row` 硬编码，
/// 而渠道只配了调度用的相对成本系数（`relative_cost_milli`），无法推出绝对成本。
/// 故 margin 字段先按公式返回（成本采集落地即生效），前端不据此出图。
pub async fn margin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();

    let sql = format!(
        "SELECT day, \
                countMerge(requests) AS requests, \
                sumMerge(amount) AS amount_micro, \
                sumMerge(original) AS original_micro, \
                sumMerge(discount) AS discount_micro, \
                sumMerge(upstream_cost) AS upstream_cost_micro, \
                sumMerge(errors) AS errors \
         FROM mv_user_day WHERE day >= today() - {days} \
         GROUP BY day ORDER BY day"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    let mut total_amount = 0_i64;
    let mut total_cost = 0_i64;
    let mut total_discount = 0_i64;
    let mut total_requests = 0_i64;
    let mut total_errors = 0_i64;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let amount = ch_i64(r, "amount_micro");
            let cost = ch_i64(r, "upstream_cost_micro");
            let discount = ch_i64(r, "discount_micro");
            total_amount = total_amount.saturating_add(amount);
            total_cost = total_cost.saturating_add(cost);
            total_discount = total_discount.saturating_add(discount);
            total_requests = total_requests.saturating_add(ch_i64(r, "requests"));
            total_errors = total_errors.saturating_add(ch_i64(r, "errors"));
            json!({
                "day": r.get("day").and_then(Value::as_str).unwrap_or_default(),
                "requests": ch_i64(r, "requests"),
                "amount_micro": amount,
                "original_micro": ch_i64(r, "original_micro"),
                "discount_micro": discount,
                "upstream_cost_micro": cost,
                "margin_micro": amount.saturating_sub(cost),
            })
        })
        .collect();

    Ok(Json(json!({
        "days": days,
        "data": data,
        "total": {
            "requests": total_requests,
            "errors": total_errors,
            "error_rate_bp": rate_bp(total_errors, total_requests),
            "amount_micro": total_amount,
            "discount_micro": total_discount,
            "upstream_cost_micro": total_cost,
            "margin_micro": total_amount.saturating_sub(total_cost),
            "margin_rate_bp": rate_bp(total_amount.saturating_sub(total_cost), total_amount),
        },
    })))
}
