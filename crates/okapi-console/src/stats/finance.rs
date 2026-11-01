//! 分组收支、现金流与毛利。
use super::super::query::Query;
use super::{WindowQuery, apply_cost_coverage, ch_i64, ch_or_disabled, rate_bp};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use serde_json::{Value, json};
use std::collections::HashMap;

/// GET /admin/stats/groups：分组经营（mv_group_day——此前仅 MCP usage_stats 可查，
/// 控制面无出口）。价格分组是站长的商业分层（free / default / vip），
/// "哪个分组在贡献收入、哪个分组错误率高"是运营决策的直接输入。
pub async fn groups(
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

    let correction = super::super::input_units::correction_source("group_code", &range);
    let tokens = super::super::input_units::corrected_sql(
        "sumMerge(tokens)",
        "max(ifNull(c.legacy_characters,0))",
    );
    let sql = format!(
        "SELECT group_code, countMerge(requests) AS reqs, {tokens} AS toks, \
                sumMerge(amount) AS spend, sumMerge(discount) AS saved, sumMerge(errors) AS errs, \
                sum(sumMerge(amount)) OVER () AS total_spend, count() OVER () AS total_groups \
         FROM mv_group_day LEFT JOIN {correction} c USING (group_code) WHERE {range} \
         GROUP BY group_code ORDER BY spend DESC LIMIT 50"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    // 分组倍率从 PG 补齐：同一张表里看得到"vip 倍率 0.8 却贡献六成收入"这类事
    let codes: Vec<String> = rows
        .iter()
        .map(|r| {
            r.get("group_code")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let ratios: HashMap<String, String> = sqlx::query!(
        r#"SELECT group_code, group_ratio::text AS "ratio!" FROM price_groups
           WHERE group_code = ANY($1)"#,
        &codes
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .into_iter()
    .map(|r| (r.group_code, r.ratio))
    .collect();

    let total_spend = rows.first().map_or(0, |r| ch_i64(r, "total_spend"));
    let total_groups = rows.first().map_or(0, |r| ch_i64(r, "total_groups"));
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let code = r
                .get("group_code")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let reqs = ch_i64(r, "reqs");
            let spend = ch_i64(r, "spend");
            json!({
                "group": code,
                "group_ratio": ratios.get(code).cloned(),
                "requests": reqs,
                "tokens": ch_i64(r, "toks"),
                "amount_micro": spend,
                "share_bp": rate_bp(spend, total_spend),
                "discount_micro": ch_i64(r, "saved"),
                "errors": ch_i64(r, "errs"),
                "error_rate_bp": rate_bp(ch_i64(r, "errs"), reqs),
            })
        })
        .collect();
    Ok(Json(
        json!({ "days": days, "window": calendar.json(), "limit": 50, "total_groups": total_groups, "total_amount_micro": total_spend, "data": data }),
    ))
}

/// GET /admin/stats/cashflow：资金流入概要（老 ok-api RevenueSummary 吸收）。
///
/// 看板其余端点全是**钱怎么花**（消费口径，CH）；这里回答**钱怎么进**——
/// 充值了多少（recharge = 支付网关真金白银）、送出多少（adjust 正向 =
/// 兑换码/补偿/返利入账）、过期清了多少（expire）。三类分开列：把兑换码
/// 入账混进"收入"会高估现金流，站长按"充值 − 消费"估算利润时会被误导。
///
/// 不含 `refund` 事件是有意的：它是管理员把某次**消费**退回钱包（`pg/refunds.rs`），不是充值退款
/// （支付网关没有退款入口）。消费侧的 CH 统计里同一笔退款已有 `log_type = 6` 的负额冲销行，消费额是
/// 净的；这里再记一遍，「充值 − 消费」就会把退款扣两次。
///
/// 数据源 billing_events（PG）：入账事件低频（对齐 §3.5"充值/管理落 PG"），
/// 聚合无需 CH——最小部署（无 CH）也能看到资金面，这是刻意保留的性质。
pub async fn cashflow(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let days = i32::try_from(q.days()).unwrap_or(7);

    // 符号进 GROUP BY 而非 Rust 侧拆：adjust 双向并存（正 = 兑换/补偿/返利，
    // 负 = 管理员扣减），先 SUM 再看符号拿到的是净值，扣减会被入账吞掉。
    // SUM(bigint) 在 PG 返回 numeric（防溢出语义），显式收回 bigint。
    let rows = sqlx::query!(
        r#"SELECT event_type,
                  (created_at >= date_trunc('day', now())) AS "is_today!",
                  (delta_micro > 0) AS "is_credit!",
                  SUM(delta_micro)::bigint AS "delta!: i64"
           FROM billing_events
           WHERE event_type IN ('recharge', 'adjust', 'expire')
             AND created_at >= now() - make_interval(days => $1)
             AND delta_micro <> 0
           GROUP BY event_type, 2, 3"#,
        days
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let pack = |today_only: bool| {
        let mut recharge = 0_i64;
        let mut granted = 0_i64;
        let mut clawed = 0_i64;
        let mut expired = 0_i64;
        for r in rows.iter().filter(|r| !today_only || r.is_today) {
            match (r.event_type.as_str(), r.is_credit) {
                ("recharge", _) => recharge += r.delta,
                ("adjust", true) => granted += r.delta,
                ("adjust", false) => clawed += -r.delta,
                ("expire", _) => expired += -r.delta,
                _ => {}
            }
        }
        json!({
            "recharge_micro": recharge,
            "granted_micro": granted,
            "clawed_micro": clawed,
            "expired_micro": expired,
        })
    };

    Ok(Json(json!({
        "days": q.days(),
        "today": pack(true),
        "window": pack(false),
    })))
}

pub async fn margin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let window =
        super::super::usage_details::CalendarWindow::read(ch, q.days(), None, None).await?;
    let days = window.days();
    let range = window.day_filter();

    let sql = format!(
        "SELECT day, \
                countMerge(requests) AS requests, countMerge(financial_records) AS financial_records, \
                sumMerge(amount) AS amount_micro, \
                sumMerge(original) AS original_micro, \
                sumMerge(discount) AS discount_micro, \
                sumMerge(upstream_cost) AS upstream_cost_micro, \
                sumMerge(errors) AS errors \
         FROM mv_user_day WHERE {range} \
         GROUP BY day ORDER BY day"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    let coverage = ch.query_json_each_row(&format!(
        "SELECT toDate(hour) AS day, countIfMerge(cost_known) AS known, sumMerge(known_amount) AS revenue, sumMerge(known_cost) AS cost FROM mv_analysis_hour WHERE {range} GROUP BY day"
    )).await?;
    let mut known_totals = [0_i64; 3];
    let mut total_amount = 0_i64;
    let mut total_cost = 0_i64;
    let mut total_discount = 0_i64;
    let mut total_requests = 0_i64;
    let mut total_records = 0_i64;
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
            total_records = total_records.saturating_add(ch_i64(r, "financial_records"));
            total_errors = total_errors.saturating_add(ch_i64(r, "errors"));
            let day = r.get("day").and_then(Value::as_str).unwrap_or_default();
            let known = coverage.iter().find(|c| c["day"] == day);
            let values =
                ["known", "revenue", "cost"].map(|field| known.map_or(0, |c| ch_i64(c, field)));
            for (acc, v) in known_totals.iter_mut().zip(values) {
                *acc = acc.saturating_add(v);
            }
            let mut row = json!({
                "day": r.get("day").and_then(Value::as_str).unwrap_or_default(),
                "requests": ch_i64(r, "requests"),
                "amount_micro": amount,
                "original_micro": ch_i64(r, "original_micro"),
                "discount_micro": discount,
                "upstream_cost_micro": cost,
            });
            apply_cost_coverage(&mut row, ch_i64(r, "financial_records"), values);
            row
        })
        .collect();

    let mut result = json!({
        "days": days,
        "window": window.json(),
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
    });
    apply_cost_coverage(&mut result["total"], total_records, known_totals);
    result["window"]["freshness"] = super::super::analysis_freshness::read(&state).await?;
    Ok(Json(result))
}
