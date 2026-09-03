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
pub(super) fn ch_i64(row: &Value, key: &str) -> i64 {
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
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let hours = q.hours.unwrap_or(24).clamp(1, 168);
    let id = id.max(0);

    let ttft = quantile_cols("ttft_q", "ttft");
    // 别名不与 MV 原始列同名（ts5 → bucket、errors → errs），GROUP/ORDER 用原始列
    let sql = format!(
        "SELECT toString(ts5) AS bucket, \
                countMerge(requests) AS reqs, \
                sumMerge(errors) AS errs, \
                {ttft}, \
                sumMerge(failovers) AS fo, \
                sumMerge(completion_tokens_sum) AS completion_tokens, \
                sumMerge(latency_sum) AS latency_ms_sum \
         FROM mv_channel_5min \
         WHERE channel_id = {id} AND ts5 >= now() - INTERVAL {hours} HOUR \
         GROUP BY ts5 ORDER BY ts5"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    let mut total_reqs = 0_i64;
    let mut total_errs = 0_i64;
    let data: Vec<Value> = rows
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
                "ttft_p50_ms": ch_i64(r, "ttft_p50_ms"),
                "ttft_p95_ms": ch_i64(r, "ttft_p95_ms"),
                "failovers": ch_i64(r, "fo"),
                "tokens_per_1k_sec": tokens_per_1k_sec(r),
            })
        })
        .collect();

    Ok(Json(json!({
        "channel_id": id,
        "hours": hours,
        "requests": total_reqs,
        "errors": total_errs,
        "error_rate_bp": rate_bp(total_errs, total_reqs),
        "data": data,
    })))
}

/// 占比 → 基点（万分之一；整数运算，分母 0 返 0）。
pub(super) fn rate_bp(part: i64, total: i64) -> i64 {
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
/// overview 今日档与窗口档共用的列集合。`uniqExact(user_id)` 只能在 MV 的原始维度列上
/// 求值，故两档都直查 mv_user_day 而非在其上二次聚合。
const OVERVIEW_COLS: &str = "countMerge(requests) AS requests, \
                             sumMerge(tokens) AS tokens, \
                             sumMerge(amount) AS amount_micro, \
                             sumMerge(original) AS original_micro, \
                             sumMerge(discount) AS discount_micro, \
                             sumMerge(upstream_cost) AS upstream_cost_micro, \
                             sumMerge(errors) AS errors, \
                             uniqExact(user_id) AS active_users";

/// GET /admin/stats/overview：站点即时 KPI（今日 / 窗口双档）。
///
/// 与 `margin` 的分工：margin 是**按日明细 + 毛利率**（趋势图数据源），
/// overview 是**单屏概览数字**（含活跃用户数，margin 未覆盖），
/// 两者同源 mv_user_day，各取所需，不重复聚合逻辑。
pub async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();

    let today_sql = format!("SELECT {OVERVIEW_COLS} FROM mv_user_day WHERE day = today()");
    // 昨日同档：单看"今日 1485 次请求"无法判断好坏，环比才是运营真正读的那个数。
    // 取整日而非"昨日同一时刻"——同比对齐时刻要按小时聚合，mv_user_day 是日粒度；
    // 前端据此标注为「昨日全天」，不假装是等时长对比。
    let yesterday_sql = format!("SELECT {OVERVIEW_COLS} FROM mv_user_day WHERE day = today() - 1");
    let window_sql =
        format!("SELECT {OVERVIEW_COLS} FROM mv_user_day WHERE day >= today() - {days}");
    let today = ch.query_json_each_row(&today_sql).await?;
    let yesterday = ch.query_json_each_row(&yesterday_sql).await?;
    let window = ch.query_json_each_row(&window_sql).await?;

    let pack = |rows: &[Value]| {
        let Some(r) = rows.first() else {
            return json!({});
        };
        let amount = ch_i64(r, "amount_micro");
        let cost = ch_i64(r, "upstream_cost_micro");
        let requests = ch_i64(r, "requests");
        let errors = ch_i64(r, "errors");
        json!({
            "requests": requests,
            "errors": errors,
            "error_rate_bp": rate_bp(errors, requests),
            "tokens": ch_i64(r, "tokens"),
            "amount_micro": amount,
            "original_micro": ch_i64(r, "original_micro"),
            "discount_micro": ch_i64(r, "discount_micro"),
            "upstream_cost_micro": cost,
            // 毛利 = 实付 − 上游成本；可为负（折扣过深/上游涨价的告警信号）
            "margin_micro": amount.saturating_sub(cost),
            "margin_rate_bp": rate_bp(amount.saturating_sub(cost), amount),
            "active_users": ch_i64(r, "active_users"),
        })
    };

    Ok(Json(json!({
        "days": days,
        "today": pack(&today),
        "yesterday": pack(&yesterday),
        "window": pack(&window),
    })))
}

/// GET /admin/stats/realtime：秒级实时 KPI（Redis 秒桶，完全不碰 CH）。
///
/// 这是 MV 看板答不了的那一档：MV 随 chsink 批写增量刷新，最快也是 1–3s 后可见，
/// 且最细粒度是 5min（mv_channel_5min）。发布新价、封一条渠道、被刷量——
/// 这些时刻站长要的是"此刻每秒多少请求"，秒桶正是为此而存在
/// （DESIGN §5 已把它列为数据源，此前一直没有写入方与出口）。
///
/// CH 未启用也照常工作：实时档只依赖 Redis，而 Redis 是必须组件。
pub async fn realtime(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RealtimeQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let window = q.window();
    let series = state.sched.kpi_window(window).await;

    let requests: i64 = series.iter().map(|s| s.requests).sum();
    let errors: i64 = series.iter().map(|s| s.errors).sum();
    let tokens: i64 = series.iter().map(|s| s.tokens).sum();
    let amount: i64 = series.iter().map(|s| s.amount_micro).sum();
    // QPS 取窗口末尾 10 秒的均值：单秒采样在中小站点抖动过大（一秒 3 个、
    // 下一秒 0 个），10 秒平滑后既跟得上变化又不至于让数字乱跳。
    let tail: i64 = series.iter().rev().take(10).map(|s| s.requests).sum();
    let tail_len = i64::try_from(series.len().min(10)).unwrap_or(1).max(1);

    let points: Vec<Value> = series
        .iter()
        .map(|s| {
            json!({
                "ts": s.ts,
                "requests": s.requests,
                "tokens": s.tokens,
                "errors": s.errors,
                "amount_micro": s.amount_micro,
            })
        })
        .collect();

    Ok(Json(json!({
        "window_secs": window,
        // 千分之一请求/秒：与 error_rate_bp、tokens_per_1k_sec 同一纪律——
        // 后端只出整数，小数点由前端按 locale 决定怎么摆。
        "qps_milli": tail.saturating_mul(1_000) / tail_len,
        "requests": requests,
        "errors": errors,
        "error_rate_bp": rate_bp(errors, requests),
        "tokens": tokens,
        "amount_micro": amount,
        "series": points,
    })))
}

#[derive(Deserialize)]
pub struct RealtimeQuery {
    /// 回看秒数（1–300，缺省 60）。
    #[serde(default)]
    pub window: Option<i64>,
}

impl RealtimeQuery {
    fn window(&self) -> i64 {
        self.window
            .unwrap_or(60)
            .clamp(1, crate::gateway::sched_redis::KPI_WINDOW_MAX)
    }
}

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
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let limit = q.limit();

    // 子查询列名（errs/ustatus）刻意与外层别名错开：ClickHouse 会把
    // `argMax(channel_id, errors)` 里的 errors 解析成外层 `sum(...) AS errors`
    // 自身，报"聚合套聚合"。
    let sql = format!(
        "SELECT error_code, sum(errs) AS errors, \
                argMax(channel_id, errs) AS top_channel_id, \
                argMax(model, errs) AS top_model, \
                max(ustatus) AS upstream_status \
         FROM ( \
             SELECT error_code, channel_id, model, \
                    countMerge(errors) AS errs, \
                    maxMerge(upstream_status) AS ustatus \
             FROM mv_error_hour WHERE hour >= now() - INTERVAL {days} DAY \
             GROUP BY error_code, channel_id, model \
         ) GROUP BY error_code ORDER BY errors DESC LIMIT {limit}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    let ids: Vec<i64> = rows.iter().map(|r| ch_i64(r, "top_channel_id")).collect();
    let channel_names: HashMap<i64, String> =
        sqlx::query!(r#"SELECT id, name FROM channels WHERE id = ANY($1)"#, &ids)
            .fetch_all(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?
            .into_iter()
            .map(|r| (r.id, r.name))
            .collect();

    let total: i64 = rows.iter().map(|r| ch_i64(r, "errors")).sum();
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
    Ok(Json(json!({ "days": days, "total": total, "data": data })))
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
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let limit = q.limit();

    let sql = format!(
        "SELECT client_type, countMerge(requests) AS reqs, sumMerge(tokens) AS toks, \
                sumMerge(amount) AS spend, sumMerge(errors) AS errs, uniqMerge(users) AS uniq_users \
         FROM mv_client_day WHERE day >= today() - {days} \
         GROUP BY client_type ORDER BY reqs DESC LIMIT {limit}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    let total_requests: i64 = rows.iter().map(|r| ch_i64(r, "reqs")).sum();
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
        json!({ "days": days, "total_requests": total_requests, "data": data }),
    ))
}

/// 折叠桶名：Top N 之外的模型归并到这里（前端渲染为"其他"并配灰色）。
/// 名字带双下划线防与真实模型名撞车。
const OTHER_BUCKET: &str = "__other";

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
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
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
    let sql = format!(
        "SELECT {bucket_expr} AS bucket, model, \
                sumMerge(amount) AS spend, countMerge(requests) AS reqs \
         FROM mv_model_hour WHERE hour >= now() - INTERVAL {days} DAY \
         GROUP BY bucket, model ORDER BY bucket"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    // 窗口总消耗排序取 Top N（占比小的模型进"其他"）
    let mut totals: HashMap<String, i64> = HashMap::new();
    for r in &rows {
        let model = r.get("model").and_then(Value::as_str).unwrap_or_default();
        *totals.entry(model.to_owned()).or_default() += ch_i64(r, "spend");
    }
    let mut ranked: Vec<(String, i64)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top: Vec<String> = ranked.iter().take(limit).map(|(m, _)| m.clone()).collect();
    let has_other = ranked.len() > top.len();

    // bucket → (model → {spend, requests})，保持 SQL 的时间升序
    let mut order: Vec<String> = Vec::new();
    let mut folded: HashMap<String, HashMap<String, (i64, i64)>> = HashMap::new();
    for r in &rows {
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
        cell.0 += ch_i64(r, "spend");
        cell.1 += ch_i64(r, "reqs");
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
                    cells.get(m).map(|(spend, reqs)| {
                        (
                            m.clone(),
                            json!({ "amount_micro": spend, "requests": reqs }),
                        )
                    })
                })
                .collect();
            json!({ "bucket": bucket, "values": values })
        })
        .collect();

    Ok(Json(json!({
        "days": days,
        "granularity": granularity,
        "models": series,
        "data": data,
    })))
}

/// GET /admin/diagnose：全链路健康——PG/Redis/CH/NATS 可达、outbox 积压、DLQ 深度、
/// 冷却 key 数、PriceBook epoch。此前只有 MCP 出口（§7.2 诊断组），站长后台
/// 反而看不到；落地页"需要注意"面板据此报组件不可达与积压。
/// 权限沿用 billing.read：其中的 DLQ/outbox 都是账务链路状态。
pub async fn diagnose(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    Ok(Json(super::mcp::diagnose(&state).await?))
}

/// GET /admin/stats/groups：分组经营（mv_group_day——此前仅 MCP usage_stats 可查，
/// 控制面无出口）。价格分组是站长的商业分层（free / default / vip），
/// "哪个分组在贡献收入、哪个分组错误率高"是运营决策的直接输入。
pub async fn groups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();

    let sql = format!(
        "SELECT group_code, countMerge(requests) AS reqs, sumMerge(tokens) AS toks, \
                sumMerge(amount) AS spend, sumMerge(discount) AS saved, sumMerge(errors) AS errs \
         FROM mv_group_day WHERE day >= today() - {days} \
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

    let total_spend: i64 = rows.iter().map(|r| ch_i64(r, "spend")).sum();
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
        json!({ "days": days, "total_amount_micro": total_spend, "data": data }),
    ))
}

/// GET /admin/stats/cashflow：资金流入概要（老 ok-api RevenueSummary 吸收）。
///
/// 看板其余端点全是**钱怎么花**（消费口径，CH）；这里回答**钱怎么进**——
/// 充值了多少（recharge = 支付网关真金白银）、送出多少（adjust 正向 =
/// 兑换码/补偿/返利入账）、过期清了多少（expire）。三类分开列：把兑换码
/// 入账混进"收入"会高估现金流，站长按"充值 − 消费"估算利润时会被误导。
///
/// 数据源 billing_events（PG）：入账事件低频（对齐 §3.5"充值/管理落 PG"），
/// 聚合无需 CH——最小部署（无 CH）也能看到资金面，这是刻意保留的性质。
pub async fn cashflow(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WindowQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
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

#[derive(Deserialize)]
pub struct BreakdownQuery {
    #[serde(default)]
    pub days: Option<u32>,
    /// `key`（缺省：只看当前这把 key，合作商员工视角）| `user`（钱包主体汇总）。
    #[serde(default)]
    pub scope: Option<String>,
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
pub async fn my_breakdown(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<BreakdownQuery>,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days.unwrap_or(7).clamp(1, 90);
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
         WHERE user_id = {}{key_filter} AND day >= today() - {days} \
         GROUP BY day, model ORDER BY day, spend DESC",
        key.user_id
    );
    let rows = ch.query_json_each_row(&sql).await?;

    let mut total = [0_i64; 7]; // reqs, prompt, cached, completion, reasoning, spend, saved
    let data: Vec<Value> = rows
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
            for (acc, v) in total.iter_mut().zip(cells) {
                *acc = acc.saturating_add(v);
            }
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

    // 平均 RPM/TPM 对齐 new-api 数据看板口径：窗口总量 ÷ 窗口分钟数。
    // 用百万分位：个人用户一天 2 笔 = 0.0007/min，千分位仍截成 0。
    let minutes = i64::from(days) * 1_440;
    let tokens = total[1].saturating_add(total[3]);

    // 钱包级窗口消费（与 scope 无关，恒按用户聚合）：余额是钱包的属性，"还能撑几天"
    // 必须用整个钱包的日均消费算——合作商员工在 key 视角下若按自己那把 key 估，
    // 会把公司钱包的寿命高估好几倍。mv_user_day 主键前缀点查，≤ days 行。
    let wallet_sql = format!(
        "SELECT sumMerge(amount) AS spend FROM mv_user_day \
         WHERE user_id = {} AND day >= today() - {days}",
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
            "rpm_limit": key.rpm_limit,
            "tpm_limit": key.tpm_limit,
            "rpd_limit": key.rpd_limit,
        })
    };

    Ok(Json(json!({
        "scope": if user_scope { "user" } else { "key" },
        "days": days,
        "total": {
            "requests": total[0],
            "prompt_tokens": total[1],
            "cached_tokens": total[2],
            "completion_tokens": total[3],
            "reasoning_tokens": total[4],
            "tokens": tokens,
            "amount_micro": total[5],
            "discount_micro": total[6],
            "cache_hit_bp": rate_bp(total[2], total[1]),
            "avg_rpm_micro": total[0].saturating_mul(1_000_000) / minutes,
            "avg_tpm_micro": tokens.saturating_mul(1_000_000) / minutes,
        },
        // 钱包视角的窗口消费（不随 scope 变）：门户据此算"余额按近期日均可用约 N 天"
        "wallet_window_spend_micro": wallet_spend,
        "live": live,
        "data": data,
    })))
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
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let sql = format!(
        "SELECT day, model, countMerge(requests) AS requests, \
                sumMerge(tokens) AS tokens, sumMerge(amount) AS amount_micro, \
                sumMerge(discount) AS discount_micro \
         FROM mv_user_model_day \
         WHERE user_id = {} AND day >= today() - {days} \
         GROUP BY day, model ORDER BY day, amount_micro DESC",
        key.user_id
    );
    let rows = ch.query_json_each_row(&sql).await?;
    let data: Vec<Value> = rows
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
    Ok(Json(json!({ "days": days, "data": data })))
}

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
