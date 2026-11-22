//! 渠道健康：渠道列表的用量与成功率、单渠道用量、渠道时间线。
use super::super::query::Query;
use super::{WindowQuery, ch_i64, ch_or_disabled, page_summary, rate_bp};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

const HOURLY_SLOTS: usize = 48;
const SLOT_SECS: i64 = 1_800;

/// 构造单渠道最近 24 小时的连续 30 分钟槽（按时间升序，共 48 格，无流量的时段补零）。
fn build_hourly_slots(start_slot: i64, by_slot: Option<&HashMap<i64, (i64, i64)>>) -> Vec<Value> {
    (0..HOURLY_SLOTS)
        .map(|i| {
            let ts = start_slot + i64::try_from(i).unwrap_or(0) * SLOT_SECS;
            let (requests, errors) = by_slot.and_then(|m| m.get(&ts)).copied().unwrap_or((0, 0));
            json!({ "ts": ts, "requests": requests, "errors": errors })
        })
        .collect()
}

/// GET /admin/stats/channels：渠道健康（错误率 / TTFT 分位 / 切换 / 粘性命中；days=1 附 24h 分段成功率桶）。
/// 数据源 mv_channel_5min——此前该视图的 errors 与分位数列无任何查询出口。
#[allow(clippy::too_many_lines)]
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

    let now_ts = chrono::Utc::now().timestamp();
    let since = now_ts - i64::from(days) * 86_400;
    let hourly_start = (days == 1).then(|| {
        now_ts.div_euclid(SLOT_SECS) * SLOT_SECS
            - (i64::try_from(HOURLY_SLOTS).unwrap_or(48) - 1) * SLOT_SECS
    });
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

    // 24h 视图附带每条渠道 48 个 30 分钟时段的请求与失败计数，一次批量聚合避免前端 N+1
    let mut hourly_by_channel: HashMap<i64, HashMap<i64, (i64, i64)>> = HashMap::new();
    if let Some(start_slot) = hourly_start
        && !ids.is_empty()
    {
        let id_csv = ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let hourly_sql = format!(
            "SELECT channel_id, \
                    intDiv(toUnixTimestamp(ts5), {SLOT_SECS}) * {SLOT_SECS} AS slot_ts, \
                    countMerge(requests) AS requests, \
                    sumMerge(errors) AS errors \
             FROM mv_channel_5min \
             WHERE channel_id IN ({id_csv}) AND ts5 >= fromUnixTimestamp({start_slot}) \
             GROUP BY channel_id, slot_ts \
             ORDER BY channel_id, slot_ts"
        );
        let hourly_rows = ch
            .query_json_each_row(&hourly_sql)
            .await
            .map_err(AppError::from)?;
        for hr in &hourly_rows {
            let cid = ch_i64(hr, "channel_id");
            let slot_ts = ch_i64(hr, "slot_ts");
            let reqs = ch_i64(hr, "requests");
            let errs = ch_i64(hr, "errors");
            hourly_by_channel
                .entry(cid)
                .or_default()
                .insert(slot_ts, (reqs, errs));
        }
    }

    let mut data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id = ch_i64(r, "channel_id");
            let requests = ch_i64(r, "requests");
            let errors = ch_i64(r, "errors");
            let (name, provider) = meta.get(&id).cloned().unwrap_or_default();
            let mut row = json!({
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
            });
            if let (Some(start_hour), Some(obj)) = (hourly_start, row.as_object_mut()) {
                obj.insert(
                    "hourly".into(),
                    Value::Array(build_hourly_slots(start_hour, hourly_by_channel.get(&id))),
                );
            }
            row
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
    let mut payload = json!({
        "days": days,
        "limit": limit,
        "offset": offset,
        "total_items": ch_i64(&summary, "total_items"),
        "data": data,
    });
    if let (Some(start_hour), Some(obj)) = (hourly_start, payload.as_object_mut()) {
        obj.insert("hourly_start".into(), json!(start_hour));
    }
    Ok(Json(payload))
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
             FROM mv_analysis_5min WHERE ts5 >= toStartOfHour(fromUnixTimestamp({since})) \
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hourly_slots_fill_48_contiguous_half_hours_and_preserve_counts() {
        let start = 1_760_000_000 / SLOT_SECS * SLOT_SECS;
        let mut counts = HashMap::new();
        counts.insert(start, (10, 0));
        counts.insert(start + 5 * SLOT_SECS, (25, 2));
        counts.insert(start + 47 * SLOT_SECS, (4, 4));
        let slots = build_hourly_slots(start, Some(&counts));
        assert_eq!(slots.len(), 48);
        assert_eq!(
            slots[0],
            json!({ "ts": start, "requests": 10, "errors": 0 })
        );
        assert_eq!(
            slots[1],
            json!({ "ts": start + SLOT_SECS, "requests": 0, "errors": 0 })
        );
        assert_eq!(
            slots[5],
            json!({ "ts": start + 5 * SLOT_SECS, "requests": 25, "errors": 2 })
        );
        assert_eq!(
            slots[47],
            json!({ "ts": start + 47 * SLOT_SECS, "requests": 4, "errors": 4 })
        );
    }
}
