//! 门户：用户自己的用量拆分与每日用量。
use super::super::query::Query;
use super::{WindowQuery, ch_i64, ch_or_disabled, rate_bp, scaled_ratio};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct BreakdownQuery {
    #[serde(default)]
    pub days: Option<u32>,
    /// `key`（缺省：只看当前这把 key，合作商员工视角）| `user`（钱包主体汇总）。
    #[serde(default)]
    pub scope: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

/// GET /api/me/stats/breakdown：门户看板的单一数据源（mv_key_model_day）。
///
/// 一次查询返回 (day, model) 粒度 + token 四轴 + 金额/让利，前端由此派生
/// new-api 数据看板的全部视图（六张 KPI、按模型堆叠趋势、模型分布）以及
/// Sub2API 强项的 Token 构成（input / cache read / output / reasoning）——
/// 不再让门户首页并发三条查询各取一角。
///
/// scope 决定主键前缀：`key` 打 (user_id, api_key_id)，`user` 打 (user_id)；
/// 两者都是前缀扫描。user_id 一律取自鉴权主体，与 my_daily 同一越权防线。
#[allow(clippy::too_many_lines)]
pub async fn my_breakdown(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<BreakdownQuery>,
) -> Result<Json<Value>, AppError> {
    let key = crate::kernel::auth::authenticate(&state, &headers).await?;
    let ch = ch_or_disabled(&state)?;
    let window = super::super::usage_details::CalendarWindow::read(
        ch,
        q.days.unwrap_or(7),
        q.start_date.as_deref(),
        q.end_date.as_deref(),
    )
    .await?;
    let days = window.days();
    let range = window.day_filter();
    let user_scope = q.scope.as_deref() == Some("user");
    let key_filter = if user_scope {
        String::new()
    } else {
        format!(" AND api_key_id = {}", key.key_id)
    };

    let sql = format!(
        "SELECT day, model, countMerge(requests) AS reqs, \
                sumMerge(prompt_tokens) AS prompt, sumMerge(cached_tokens) AS cached, \
                sumMerge(completion_tokens) AS completion, sumMerge(reasoning_tokens) AS reasoning, \
                sumMerge(amount) AS spend, sumMerge(discount) AS saved, sumMerge(errors) AS errs \
         FROM mv_key_model_day \
         WHERE user_id = {}{key_filter} AND {range} \
         GROUP BY day, model ORDER BY day, spend DESC",
        key.user_id
    );
    let rows = ch.query_json_each_row(&sql).await?;

    let mut data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let cells = [
                ch_i64(r, "reqs"),
                ch_i64(r, "prompt"),
                ch_i64(r, "cached"),
                ch_i64(r, "completion"),
                ch_i64(r, "reasoning"),
                ch_i64(r, "spend"),
                ch_i64(r, "saved"),
            ];
            json!({
                "day": r.get("day").and_then(Value::as_str).unwrap_or_default(),
                "model": r.get("model").and_then(Value::as_str).unwrap_or_default(),
                "requests": cells[0],
                "prompt_tokens": cells[1],
                "cached_tokens": cells[2],
                "completion_tokens": cells[3],
                "reasoning_tokens": cells[4],
                "amount_micro": cells[5],
                "discount_micro": cells[6],
                "errors": ch_i64(r, "errs"),
            })
        })
        .collect();

    let owner = format!("user_id = {}{key_filter}", key.user_id);
    let extra = super::super::usage_details::enrich(ch, &owner, &range, &mut data).await?;
    let mut total = [0_i64; 7]; // reqs, prompt, cached, completion, reasoning, spend, saved
    for row in &data {
        for (acc, field) in total.iter_mut().zip([
            "requests",
            "prompt_tokens",
            "cached_tokens",
            "completion_tokens",
            "reasoning_tokens",
            "amount_micro",
            "discount_micro",
        ]) {
            *acc = acc.saturating_add(ch_i64(row, field));
        }
    }

    // 平均 RPM/TPM 对齐 new-api 数据看板口径：窗口总量 ÷ 窗口分钟数。
    // 用百万分位：个人用户一天 2 笔 = 0.0007/min，千分位仍截成 0。
    let minutes = days * 1_440;
    let tokens = total[1].saturating_add(total[3]);

    // 钱包级窗口消费（与 scope 无关，恒按用户聚合）：余额是钱包的属性，"还能撑几天"
    // 必须用整个钱包的日均消费算——合作商员工在 key 视角下若按自己那把 key 估，
    // 会把公司钱包的寿命高估好几倍。mv_user_day 主键前缀点查，≤ days 行。
    let wallet_sql = format!(
        "SELECT sumMerge(amount) AS spend FROM mv_user_day \
         WHERE user_id = {} AND {range}",
        key.user_id
    );
    let wallet_spend = ch
        .query_json_each_row(&wallet_sql)
        .await?
        .first()
        .map_or(0, |r| ch_i64(r, "spend"));

    // 当前速率（老 ok-api 用户页的 RPM/TPM 取法）+ 该 key 的上限：
    // 直接回答"我离限流还有多远"。只在 key 视角给——上限是 key 级属性，
    // 合作商汇总视角下没有单一上限可对照。
    let live = if user_scope {
        Value::Null
    } else {
        let (rpm, tpm, rpd) = state.sched.key_rate_snapshot(key.user_id, key.key_id).await;
        json!({
            "rpm": rpm,
            "tpm": tpm,
            "rpd": rpd,
            "rate_basis": "admission_estimate",
            "rate_window": "calendar_minute",
            "rpm_limit": key.rpm_limit,
            "tpm_limit": key.tpm_limit,
            "rpd_limit": key.rpd_limit,
        })
    };

    let errors: i64 = data.iter().map(|row| ch_i64(row, "errors")).sum();
    let mut response = json!({
        "scope": if user_scope { "user" } else { "key" },
        "days": days,
        "window": window.json(),
        "total": {
            "requests": total[0],
            "prompt_tokens": total[1],
            "cached_tokens": total[2],
            "completion_tokens": total[3],
            "reasoning_tokens": total[4],
            "tokens": tokens,
            "amount_micro": total[5],
            "discount_micro": total[6],
            "original_micro": total[5].saturating_add(total[6]),
            "errors": errors,
            "success_rate_bp": if total[0] > 0 { json!(rate_bp(total[0].saturating_sub(errors), total[0])) } else { Value::Null },
            "cache_hit_bp": rate_bp(total[2], total[1]),
            "avg_rpm_micro": scaled_ratio(total[0], minutes, 1_000_000),
            "avg_tpm_micro": scaled_ratio(tokens, minutes, 1_000_000),
            "avg_rate_window_minutes": minutes,
            "output_tps_basis": "total_request_latency",
        },
        // 钱包视角的窗口消费（不随 scope 变）：门户据此算"余额按近期日均可用约 N 天"
        "wallet_window_spend_micro": wallet_spend,
        "live": live,
        "data": data,
    });
    if let (Some(total), Some(extra)) = (response["total"].as_object_mut(), extra.as_object()) {
        total.extend(extra.clone());
    }
    Ok(Json(response))
}

/// GET /api/me/stats/daily：我的按日用量（用户门户曲线）。
///
/// 对齐 new-api「我的用量按日统计」；只看自己——user_id 取自已鉴权主体而非
/// 请求参数，从根上排除越权查他人用量。
pub async fn my_daily(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    let key = crate::kernel::auth::authenticate(&state, &headers).await?;
    let ch = ch_or_disabled(&state)?;
    let calendar =
        super::super::usage_details::CalendarWindow::read(ch, q.days(), None, None).await?;
    let days = calendar.days();
    let range = calendar.day_filter();
    let sql = format!(
        "SELECT day, model, countMerge(requests) AS requests, \
                sumMerge(tokens) AS tokens, sumMerge(amount) AS amount_micro, \
                sumMerge(discount) AS discount_micro \
         FROM mv_user_model_day \
         WHERE user_id = {} AND {range} \
         GROUP BY day, model ORDER BY day, amount_micro DESC",
        key.user_id
    );
    let rows = ch.query_json_each_row(&sql).await?;
    let mut data: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "day": r.get("day").and_then(Value::as_str).unwrap_or_default(),
                "model": r.get("model").and_then(Value::as_str).unwrap_or_default(),
                "requests": ch_i64(r, "requests"),
                "tokens": ch_i64(r, "tokens"),
                "amount_micro": ch_i64(r, "amount_micro"),
                "discount_micro": ch_i64(r, "discount_micro"),
            })
        })
        .collect();
    super::super::usage_sources::enrich(
        ch,
        "day, model",
        "mv_user_model_day",
        &format!("user_id = {} AND {range}", key.user_id),
        &mut data,
    )
    .await?;
    super::super::output_rate::enrich_model_days(
        ch,
        &mut data,
        "mv_user_model_day",
        &format!("user_id = {} AND {range}", key.user_id),
    )
    .await?;
    Ok(Json(
        json!({ "days": days, "window": calendar.json(), "data": data }),
    ))
}
