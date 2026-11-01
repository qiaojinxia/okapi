//! 首页总览（今日 / 昨日 / 七日三个窗口）与实时 KPI。
use super::super::query::Query;
use super::{apply_cost_coverage, ch_i64, ch_or_disabled, rate_bp};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use serde::Deserialize;
use serde_json::{Value, json};

/// GET /admin/stats/margin：经营口径逐日聚合（实收 / 标价 / 让利 / 毛利）。
/// 数据源 mv_user_day 的 original / discount / upstream_cost 三列，此前无查询出口。
///
/// **已知边界**：`upstream_cost` 目前全链路恒为 0——chsink `build_ch_row` 硬编码，
/// 而渠道只配了调度用的相对成本系数（`relative_cost_milli`），无法推出绝对成本。
/// 故 margin 字段先按公式返回（成本采集落地即生效），前端不据此出图。
/// overview 今日档与窗口档共用的列集合。`uniqExact(user_id)` 只能在 MV 的原始维度列上
/// 求值，故两档都直查 mv_user_day 而非在其上二次聚合。
pub(super) const OVERVIEW_COLS: &str = "countMerge(financial_records) AS financial_records,countMerge(requests) AS requests, \
                             sumMerge(tokens) AS tokens, \
                             sumMerge(amount) AS amount_micro, \
                             sumMerge(original) AS original_micro, \
                             sumMerge(discount) AS discount_micro, \
                             sumMerge(upstream_cost) AS upstream_cost_micro, \
                             sumMerge(errors) AS errors, \
                             uniqExactIf(user_id, finalizeAggregation(u.requests)>0) AS active_users";

pub(super) async fn overview_sources(
    state: &AppState,
    predicate: &str,
    cached: bool,
) -> Result<Vec<Value>, AppError> {
    // Calendar conversion may replace the daily table with an aliased subquery.
    // Give the outer facts their own alias so that conversion cannot add two aliases.
    let totals_sql =
        format!("SELECT {OVERVIEW_COLS} FROM (SELECT * FROM mv_user_day) AS u WHERE {predicate}");
    let source = super::super::token_details::with_provenance("", "mv_user_day", predicate);
    let sources_sql = format!("SELECT * FROM {source}");
    let (mut rows, sources) = tokio::try_join!(
        super::super::stats_cache::query(state, &totals_sql, &[], cached),
        super::super::stats_cache::query(state, &sources_sql, &[], cached),
    )?;
    let empty = json!({});
    if let Some(row) = rows.first_mut() {
        let source = sources.first().unwrap_or(&empty);
        super::super::input_units::correct_totals(row, source)?;
        let requests = ch_i64(row, "requests");
        let metrics = super::super::usage_sources::metrics(
            source,
            requests,
            [None, None],
            None,
            ch_i64(source, "source_read_n"),
        );
        if let Some(object) = row.as_object_mut() {
            object.extend(metrics);
            object.extend(super::super::token_details::metrics(source, requests));
        }
    }
    Ok(rows)
}

pub(super) fn pack_overview(rows: &[Value], known: [i64; 3]) -> Value {
    let Some(r) = rows.first() else {
        return json!({});
    };
    let amount = ch_i64(r, "amount_micro");
    let cost = ch_i64(r, "upstream_cost_micro");
    let requests = ch_i64(r, "requests");
    let errors = ch_i64(r, "errors");
    let mut result = json!({
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
    });
    if let Some(object) = result.as_object_mut() {
        for field in [
            "token_usage_basis",
            "token_provenance",
            "cache_hit_bp",
            "cache_hit_basis",
            "measured_cache_hit_bp",
            "measured_cache_hit_requests",
            "measured_cache_hit_coverage_bp",
            "settled_cache_hit_bp",
            "token_detail_observations",
            "token_detail_history",
            "token_detail_basis",
            "input_units",
        ] {
            object.insert(field.to_owned(), r[field].clone());
        }
    }
    apply_cost_coverage(&mut result, ch_i64(r, "financial_records"), known);
    result
}

#[derive(Deserialize, Default)]
pub struct OverviewQuery {
    #[serde(default)]
    pub cached: bool,
    pub days: Option<u32>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

/// GET /admin/stats/overview：所选日历窗口 KPI，另附今日与昨日全天参考。
/// 与 `margin` 的按日趋势同源；日期校验、时区及边界复用分析页的口径。
pub async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OverviewQuery>,
) -> Result<Json<Value>, AppError> {
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let calendar = super::super::usage_details::CalendarWindow::read(
        ch,
        q.days.unwrap_or(7).clamp(1, 366),
        q.start_date.as_deref(),
        q.end_date.as_deref(),
    )
    .await?;
    let days = calendar.days();
    let range = calendar.day_filter();

    // 取实际昨日全天，而非自定义区间终点的前一天；仅作参考，不计算今日涨跌。
    let cached = q.cached && super::super::stats_cache::allowed(&headers);
    let today_date = chrono::NaiveDate::parse_from_str(&calendar.today, "%Y-%m-%d")
        .map_err(|_| AppError::internal())?;
    let yesterday_date = (today_date - chrono::Days::new(1)).to_string();
    let today_filter = format!("day = toDate('{}')", calendar.today);
    let yesterday_filter = format!("day = toDate('{yesterday_date}')");
    let coverage_sql = format!(
        "SELECT toDate(hour) AS day, countIfMerge(cost_known) AS known, sumMerge(known_amount) AS revenue, sumMerge(known_cost) AS cost FROM mv_analysis_hour WHERE {range} OR {today_filter} OR {yesterday_filter} GROUP BY day"
    );
    let (today, yesterday, window, coverage) = tokio::try_join!(
        overview_sources(&state, &today_filter, cached),
        overview_sources(&state, &yesterday_filter, cached),
        overview_sources(&state, &range, cached),
        super::super::stats_cache::query(&state, &coverage_sql, &[], cached),
    )?;
    let known_for = |start: &str, end: &str| {
        let mut sums = [0_i64; 3];
        for row in coverage
            .iter()
            .filter(|r| r["day"].as_str().is_some_and(|d| d >= start && d <= end))
        {
            for (sum, key) in sums.iter_mut().zip(["known", "revenue", "cost"]) {
                *sum = sum.saturating_add(ch_i64(row, key));
            }
        }
        sums
    };

    Ok(Json(json!({
        "days": days,
        "calendar": calendar.json(),
        "today": pack_overview(&today, known_for(&calendar.today, &calendar.today)),
        "yesterday": pack_overview(&yesterday, known_for(&yesterday_date, &yesterday_date)),
        "window": pack_overview(&window, known_for(&calendar.start.to_string(), &calendar.end.to_string())),
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
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
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
            .clamp(1, okapi_kernel::sched_redis::KPI_WINDOW_MAX)
    }
}
