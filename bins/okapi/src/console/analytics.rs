//! 用量分析（IMPLEMENTATION §11.13）：带维度过滤的趋势 / 拆分 / 流向，三个端点
//! 同吃 `mv_cube_hour`；另含站点规模（PG）与列表行内用量（单维 MV）。
//!
//! 与 `stats.rs` 的分工：stats 的每个端点各答一个固定问题（渠道健康、模型分位、
//! 错误分布……），这里答"**限定到某个用户 / 渠道 / 模型之后**，钱和流量怎么分、
//! 随时间怎么走"。new-api #7150（看板要能用日志页的过滤条件）与 Sub2API
//! `TrendParams`（user / api_key / model / account / group 全维过滤）说的都是这件事。
//!
//! SQL 纪律同 logs.rs：整数 clamp 后进 SQL，字符串（模型名 / 分组码）走服务端绑定参数。
//! 聚合别名一律不与 MV 原始列同名（CH 的 WHERE / 聚合参数优先解析 SELECT 别名）。

use super::query::Query;
use super::stats::{ch_i64, rate_bp};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use chrono::Days;
use okapi_api::{codes, permissions};
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

fn ch_or_disabled(state: &AppState) -> Result<&ChClient, AppError> {
    state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))
}

fn ch_str<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// 去空白；空串视为未提供。
fn trimmed(field: Option<&str>) -> Option<&str> {
    field.map(str::trim).filter(|s| !s.is_empty())
}

/// 立方体查询参数：时间窗 + 至多五个维度过滤（可组合）。
#[derive(Deserialize, Default)]
pub struct CubeQuery {
    /// Dashboard-only short cache; detailed analytics default to fresh reads.
    #[serde(default)]
    pub cached: bool,
    /// 回看天数（1–90，缺省 7）。
    #[serde(default)]
    pub days: Option<u32>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub granularity: Option<String>,
    pub model_source: Option<String>,
    pub endpoint: Option<String>,
    pub upstream_endpoint: Option<String>,
    pub node: Option<String>,
    pub stream: Option<bool>,
    pub request_type: Option<String>,
    pub billing_type: Option<String>,
    /// JSON 数组：同时比较多个模型或分组，避免逗号被误当成模型名分隔符。
    pub models: Option<String>,
    pub groups: Option<String>,
    #[serde(skip)]
    start: String,
    #[serde(skip)]
    end: String,
    #[serde(skip)]
    previous_start: String,
    #[serde(skip)]
    window_meta: Value,
    #[serde(skip)]
    coverage: super::measurement_coverage::Coverage,
    #[serde(skip)]
    previous_coverage: super::measurement_coverage::Coverage,
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub api_key_id: Option<i64>,
    #[serde(default)]
    pub channel_id: Option<i64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    /// 拆分维度（breakdown 专用）：model | channel | provider | user | api_key | group。
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// 排行排序 / 流向图度量：amount | requests | tokens。
    #[serde(default)]
    pub metric: Option<String>,
    /// 趋势堆叠维度（trend 专用）：model | channel | group | user | api_key。
    #[serde(default)]
    pub stack: Option<String>,
    pub stages: Option<String>,
    /// `core`：只返回请求、Token、金额、错误等核心指标，走精简数据源（毫秒级）；
    /// 缺省 / `all` 返回全部指标。过滤或口径不允许走精简源时自动回退为全部指标。
    #[serde(default)]
    pub fields: Option<String>,
    /// 是否同时查上一窗口（环比、上期名次）。缺省 true；不展示环比的调用方传 false，
    /// 省掉整条上期查询。
    #[serde(default)]
    pub compare: Option<bool>,
}

/// 编译后的过滤：整数已进 SQL，字符串留在绑定参数里。
struct Scope {
    clause: String,
    params: Vec<(String, String)>,
}

impl Scope {
    fn borrow(&self) -> Vec<(&str, &str)> {
        self.params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

impl CubeQuery {
    fn days(&self) -> u32 {
        self.days.unwrap_or(7).clamp(1, 366)
    }

    async fn prepare(&mut self, state: &AppState) -> Result<(), AppError> {
        let ch = ch_or_disabled(state)?;
        self.core_requested()?;
        let w = super::usage_details::CalendarWindow::read(
            ch,
            self.days(),
            self.start_date.as_deref(),
            self.end_date.as_deref(),
        )
        .await?;
        let granularity =
            self.granularity
                .as_deref()
                .unwrap_or(if w.days() <= 2 { "hour" } else { "day" });
        if !matches!(granularity, "hour" | "day") || (granularity == "hour" && w.days() > 31) {
            return Err(AppError::bad_request().with_param("granularity"));
        }
        self.model_column()?;
        for (name, input) in [("models", &self.models), ("groups", &self.groups)] {
            parse_choices(name, input.as_deref())?;
        }
        if self
            .request_type
            .as_deref()
            .is_some_and(|s| !matches!(s, "stream" | "non_stream" | "websocket"))
        {
            return Err(AppError::bad_request().with_param("request_type"));
        }
        self.days = Some(u32::try_from(w.days()).unwrap_or(7));
        self.granularity = Some(granularity.to_owned());
        self.start = w.start.to_string();
        self.end = (w.end + Days::new(1)).to_string();
        self.previous_start = (w.start - Days::new(u64::from(self.days())))
            .max(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
            .to_string();
        self.window_meta = w.json();
        self.window_meta["start_at"] = json!(format!("{} 00:00:00", self.start));
        self.window_meta["end_at"] = json!(if w.end.to_string() == w.today {
            w.generated_at.clone()
        } else {
            format!("{} 23:00:00", w.end)
        });
        self.window_meta["previous_start_date"] = json!(self.previous_start);
        self.window_meta["previous_end_date"] = json!((w.start - Days::new(1)).to_string());
        // A missing current-period aggregate must not expand historical recovery
        // for a complete previous period (including an empty previous period).
        let predicate = format!("{}{}", self.window(false), self.base_scope());
        self.coverage = if self.core_candidate() {
            // 精简查询只需要知道有没有历史字符口径；其余测量口径的覆盖探测只服务完整源。
            let core =
                super::measurement_coverage::Coverage::read_historical(state, &predicate).await?;
            if core.historical_units {
                // 有历史字符口径时 prompt_tokens 要校正，必须回退完整源，需要完整覆盖探测。
                super::measurement_coverage::Coverage::read(state, &predicate, self.cached).await?
            } else {
                core
            }
        } else {
            super::measurement_coverage::Coverage::read(state, &predicate, self.cached).await?
        };
        if self.compares() {
            let predicate = format!("{}{}", self.window(true), self.base_scope());
            self.previous_coverage =
                super::measurement_coverage::Coverage::read(state, &predicate, self.cached).await?;
        }
        Ok(())
    }

    /// `fields` 取值校验；非法值在发出任何查询之前 400。
    fn core_requested(&self) -> Result<bool, AppError> {
        match trimmed(self.fields.as_deref()) {
            None | Some("all") => Ok(false),
            Some("core") => Ok(true),
            Some(_) => Err(AppError::bad_request().with_param("fields")),
        }
    }

    /// 精简查询（`fields=core`）从不查上一窗口：它既不带环比，也没有测量口径可对比。
    fn compares(&self) -> bool {
        self.compare.unwrap_or(true) && trimmed(self.fields.as_deref()) != Some("core")
    }

    /// 精简查询的静态条件：与 `core_source_ok` 的区别只在"窗口里有没有历史字符口径"——
    /// 那要读一次探测才知道，所以 `prepare` 先按这里的条件决定读哪种覆盖探测。
    fn core_candidate(&self) -> bool {
        let by = self.by.as_deref().unwrap_or("model");
        self.core_requested().unwrap_or(false)
            && !self.has_detail_filter()
            && self.model_column().is_ok_and(|column| column == "model")
            && matches!(self.stack_column(), Ok(None))
            && breakdown_key(by).is_ok_and(primary_key_column)
    }

    /// 精简源只含主键维度的核心列。没有明细过滤、拆分维度属于主键、没有历史字符口径
    /// （`prompt_tokens` 需要校正）时，它在这些列上与完整源逐值相同；否则必须回退完整源。
    fn core_source_ok(&self, key_col: Option<&str>, previous: bool) -> bool {
        !self.has_detail_filter()
            && self.model_column().is_ok_and(|column| column == "model")
            && key_col.is_none_or(primary_key_column)
            && !self.coverage_for(previous).historical_units
    }

    /// 上一窗口的汇总，只服务环比；调用方不展示环比（compare=false）时整条不查。
    async fn previous_totals(
        &self,
        state: &AppState,
        agg: &str,
        scope: &Scope,
    ) -> Result<Vec<Value>, AppError> {
        if !self.compares() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {agg} FROM {} WHERE {}{}",
            self.source(true),
            self.window(true),
            scope.clause
        );
        super::stats_cache::query(state, &sql, &scope.borrow(), self.cached).await
    }

    fn core_source(&self, previous: bool) -> String {
        super::core_source::source(&self.window(previous), &self.base_scope())
    }

    fn model_column(&self) -> Result<&'static str, AppError> {
        match self.model_source.as_deref().unwrap_or("billed") {
            "billed" => Ok("model"),
            "requested" => Ok("requested_model"),
            "upstream" => Ok("upstream_model"),
            _ => Err(AppError::bad_request().with_param("model_source")),
        }
    }

    fn stack_column(&self) -> Result<Option<String>, AppError> {
        trimmed(self.stack.as_deref())
            .map(|dimension| match dimension {
                "model" => self.model_column().map(str::to_owned),
                "model_group" => self
                    .model_column()
                    .map(|column| format!("toJSONString(tuple({column}, group_code))")),
                _ => breakdown_key(dimension).map(str::to_owned),
            })
            .transpose()
    }

    fn base_scope(&self) -> String {
        use std::fmt::Write as _;
        // 在聚合展开前裁剪常用主键维度。高级维度在外层过滤，历史未采集部分仍保留。
        let mut base = String::new();
        for (col, val) in [
            ("user_id", self.user_id),
            ("api_key_id", self.api_key_id),
            ("channel_id", self.channel_id),
        ] {
            if let Some(v) = val.filter(|v| *v >= 0) {
                let _ = write!(base, " AND {col} = {v}");
            }
        }
        base
    }

    fn source(&self, previous: bool) -> String {
        super::analysis_source::source_with_coverage(
            &self.window(previous),
            &self.base_scope(),
            self.coverage_for(previous),
        )
    }

    fn coverage_for(&self, previous: bool) -> super::measurement_coverage::Coverage {
        if previous {
            self.previous_coverage
        } else {
            self.coverage
        }
    }

    fn scope(&self) -> Scope {
        use std::fmt::Write as _;
        let mut clause = String::new();
        let mut params = Vec::new();
        for (col, v) in [
            ("user_id", self.user_id),
            ("api_key_id", self.api_key_id),
            ("channel_id", self.channel_id),
        ] {
            if let Some(v) = v.filter(|v| *v >= 0) {
                let _ = write!(clause, " AND {col} = {v}");
            }
        }
        if let Some(m) = trimmed(self.model.as_deref()) {
            let _ = write!(
                clause,
                " AND {} = {{p_model:String}}",
                self.model_column().unwrap_or("model")
            );
            params.push(("p_model".to_owned(), m.to_owned()));
        }
        if let Some(g) = trimmed(self.group.as_deref()) {
            clause.push_str(" AND group_code = {p_group:String}");
            params.push(("p_group".to_owned(), g.to_owned()));
        }
        for (col, value) in [
            ("endpoint", &self.endpoint),
            ("upstream_endpoint", &self.upstream_endpoint),
            ("node", &self.node),
            ("request_type", &self.request_type),
            ("billing_type", &self.billing_type),
        ] {
            if let Some(value) = trimmed(value.as_deref()) {
                let _ = write!(clause, " AND {col} = {{p_{col}:String}}");
                params.push((format!("p_{col}"), value.to_owned()));
            }
        }
        if let Some(stream) = self.stream {
            let _ = write!(clause, " AND stream = {}", u8::from(stream));
        }
        for (name, col, values) in [
            (
                "models",
                self.model_column().unwrap_or("model"),
                &self.models,
            ),
            ("groups", "group_code", &self.groups),
        ] {
            if let Ok(values) = parse_choices(name, values.as_deref())
                && !values.is_empty()
            {
                let binds = values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| {
                        let key = format!("p_{name}_{i}");
                        params.push((key.clone(), value.clone()));
                        format!("{{{key}:String}}")
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let _ = write!(clause, " AND {col} IN ({binds})");
            }
        }
        Scope { clause, params }
    }

    /// Old day totals may be recoverable even when their hourly allocation is not.
    async fn recover_cache(
        &self,
        state: &AppState,
        previous: bool,
        time: Option<&str>,
        dimension: Option<&str>,
        rows: &mut [Value],
    ) -> Result<(), AppError> {
        if rows.is_empty()
            || self.coverage_for(previous).legacy.cache
                != super::measurement_coverage::Mode::Recover
        {
            return Ok(());
        }
        let mut keys = Vec::new();
        let mut projections = Vec::new();
        let mut match_columns = Vec::new();
        if let Some(time) = time {
            keys.push(time);
            projections.push(format!("toString({time}) AS bucket"));
            match_columns.push("bucket");
        }
        if let Some(dimension) = dimension {
            if let Some(tuple) = dimension
                .strip_prefix("toJSONString(tuple(")
                .and_then(|s| s.strip_suffix("))"))
            {
                keys.extend(tuple.split(", "));
            } else {
                keys.push(dimension);
            }
            projections.push(format!("{dimension} AS k"));
            match_columns.push("k");
        }
        let keys = keys.join(", ");
        let (sql, scope) =
            self.cache_measurement_query(&keys, &projections.join(", "), previous, "");
        let recovered =
            super::stats_cache::query(state, &sql, &scope.borrow(), self.cached).await?;
        let key = |row: &Value| {
            match_columns
                .iter()
                .map(|column| {
                    row[*column]
                        .as_str()
                        .map_or_else(|| row[*column].to_string(), str::to_owned)
                })
                .collect::<Vec<_>>()
        };
        let recovered: HashMap<_, _> = recovered.into_iter().map(|row| (key(&row), row)).collect();
        for row in rows {
            if let Some(cache) = recovered
                .get(&key(row))
                .filter(|cache| ch_i64(cache, "cache_expected") == ch_i64(row, "reqs"))
            {
                row["write_sum"] = cache["cache_writes"].clone();
                row["write_n"] = cache["cache_write_n"].clone();
                row["read_n"] = cache["cache_read_n"].clone();
                for field in super::usage_sources::FIELDS {
                    row[field] = cache[field].clone();
                }
            }
        }
        Ok(())
    }

    fn cache_measurement_query(
        &self,
        keys: &str,
        projections: &str,
        previous: bool,
        extra: &str,
    ) -> (String, Scope) {
        let scope = self.scope();
        let predicate = format!("{}{}{extra}", self.window(previous), scope.clause);
        let table = if self.has_detail_filter()
            || keys.split(", ").any(|key| {
                !matches!(
                    key,
                    "" | "hour"
                        | "day"
                        | "model"
                        | "user_id"
                        | "api_key_id"
                        | "channel_id"
                        | "group_code"
                )
            }) {
            "mv_analysis_hour"
        } else {
            "mv_cube_hour"
        };
        let cache = super::cache_usage::source(keys, table, &predicate);
        let usage = super::usage_sources::source(keys, table, &predicate);
        let join_keys = if keys.is_empty() {
            "source_scope"
        } else {
            keys
        };
        let projections = if projections.is_empty() {
            String::new()
        } else {
            format!("{projections}, ")
        };
        let sql = format!(
            "SELECT {projections}cs.cache_expected, cs.cache_writes, cs.cache_write_n, cs.cache_read_n, {} FROM {cache} cs LEFT JOIN {usage} us USING ({join_keys})",
            super::usage_sources::FIELDS
                .map(|field| format!("us.{field} AS {field}"))
                .join(", ")
        );
        (sql, scope)
    }

    async fn recover_provider_cache(
        &self,
        state: &AppState,
        buckets: &mut [Bucket],
        rows: &[Value],
        names: &Names,
    ) -> Result<(), AppError> {
        if self.coverage.legacy.cache != super::measurement_coverage::Mode::Recover {
            return Ok(());
        }
        for bucket in buckets {
            let ids = rows
                .iter()
                .filter_map(|row| row_key(row).parse::<i64>().ok())
                .filter(|id| {
                    *id >= 0
                        && names.channels.get(id).map_or("unknown", |c| c.1.as_str()) == bucket.key
                })
                .map(|id| id.to_string())
                .collect::<Vec<_>>();
            if ids.is_empty() {
                continue;
            }
            let (sql, scope) = self.cache_measurement_query(
                "",
                "",
                false,
                &format!(" AND channel_id IN ({})", ids.join(",")),
            );
            let measured =
                super::stats_cache::query(state, &sql, &scope.borrow(), self.cached).await?;
            let requests = bucket.metrics["requests"].as_i64().unwrap_or(0);
            if let Some(measured) = measured
                .first()
                .filter(|row| ch_i64(row, "cache_expected") == requests)
            {
                let counters = json!({"write_n":measured["cache_write_n"], "read_n":measured["cache_read_n"], "write_sum":measured["cache_writes"]});
                bucket.metrics.extend(cache_metrics(&counters, requests));
                bucket.metrics.extend(super::usage_sources::metrics(
                    measured,
                    requests,
                    [
                        bucket.metrics["prompt_tokens"].as_i64(),
                        bucket.metrics["completion_tokens"].as_i64(),
                    ],
                    bucket.metrics["cached_tokens"].as_i64(),
                    ch_i64(measured, "cache_read_n"),
                ));
            }
        }
        Ok(())
    }

    async fn trend_totals(
        &self,
        state: &AppState,
        dimension: Option<&str>,
        rows: &mut [Value],
        previous: &mut [Value],
    ) -> Result<Vec<Value>, AppError> {
        self.recover_cache(state, false, self.granularity.as_deref(), dimension, rows)
            .await?;
        let mut total = vec![sum_metric_rows(rows)];
        self.recover_cache(state, false, None, None, &mut total)
            .await?;
        self.recover_cache(state, true, None, None, previous)
            .await?;
        Ok(total)
    }

    fn has_detail_filter(&self) -> bool {
        self.stream.is_some()
            || [
                &self.endpoint,
                &self.upstream_endpoint,
                &self.node,
                &self.request_type,
                &self.billing_type,
            ]
            .iter()
            .any(|value| trimmed(value.as_deref()).is_some())
            || (self.model_column().is_ok_and(|col| col != "model")
                && (trimmed(self.model.as_deref()).is_some()
                    || trimmed(self.models.as_deref()).is_some()))
    }

    /// 当前窗口 / 等长的上一窗口。环比不是"昨日"那种整日锚点，而是同长度的
    /// 前一段：7 天看板对 7 天，30 天对 30 天，否则周末效应会把对比读歪。
    fn window(&self, previous: bool) -> String {
        let (start, end) = if previous {
            (&self.previous_start, &self.start)
        } else {
            (&self.start, &self.end)
        };
        format!("hour >= toDateTime('{start}') AND hour < toDateTime('{end}')")
    }
}

/// 立方体全部度量的 Merge 列表；别名刻意与 MV 列名错开。
const AGG: &str = "sum(financial_records) AS financial_records, sum(requests) AS reqs, \
                   sum(prompt_tokens) AS prompt, \
                   sum(cached_tokens) AS cached, \
                   sum(completion_tokens) AS completion, \
                   sum(reasoning_tokens) AS reasoning, \
                   sum(amount) AS spend, \
                   sum(discount) AS saved, \
                   sum(upstream_cost) AS cost, \
                   sum(errors) AS errs, \
                   sum(latency_sum) AS lat_sum, sum(latency_samples) AS lat_n, \
                   sum(latency_observed) AS lat_observed, sum(latency_output) AS lat_output, \
                   sum(ttft_sum) AS ttft_s, \
                   sum(ttft_samples) AS ttft_n, sum(ttft_observed) AS ttft_observed, \
                   sum(write_tokens) AS write_sum, sum(write_samples) AS write_n, \
                   sum(read_samples) AS read_n, \
                   sum(cost_samples) AS cost_n, sum(covered_amount) AS covered_spend, sum(covered_cost) AS covered_cost_sum";

fn aggregate_metrics() -> String {
    // This is a hint, not an authoritative test flag. Keep all matching records
    // in every total and require the fixture's exact random-suffix format.
    const FIXTURE_MODEL: &str = "^(cube-[ab]-[0-9a-f]{10}|log-[0-9a-f]{12}|modal-[0-9a-f]{32})$";
    format!(
        "{AGG}, sumIf(requests, match(model, '{FIXTURE_MODEL}')) AS fixture_reqs, \
        sumIf(prompt_tokens + completion_tokens, match(model, '{FIXTURE_MODEL}')) AS fixture_tokens, {}, {}, {}",
        super::usage_sources::sum_sql(),
        super::token_details::sum_sql(),
        super::output_rate::sum_sql()
    )
}

/// 一行聚合 → 展示字段（比率全部基点/整数，避免前端拿浮点二次换算）。
/// Bucket counters are additive; ratios and averages are packed only after summing.
/// This avoids querying the entire current source again just for the trend total.
fn sum_metric_rows(rows: &[Value]) -> Value {
    let mut total = json!({});
    for row in rows {
        for key in [
            "reqs",
            "financial_records",
            "prompt",
            "cached",
            "completion",
            "reasoning",
            "spend",
            "saved",
            "cost",
            "errs",
            "lat_sum",
            "lat_n",
            "lat_observed",
            "lat_output",
            "ttft_s",
            "ttft_n",
            "ttft_observed",
            "write_sum",
            "write_n",
            "read_n",
            "cost_n",
            "covered_spend",
            "covered_cost_sum",
            "fixture_reqs",
            "fixture_tokens",
        ] {
            total[key] = json!(ch_i64(&total, key).saturating_add(ch_i64(row, key)));
        }
        super::usage_sources::accumulate(&mut total, row);
        super::token_details::accumulate(&mut total, row);
        super::output_rate::accumulate(&mut total, row);
    }
    total
}

fn pack_metrics(r: &Value) -> serde_json::Map<String, Value> {
    let reqs = ch_i64(r, "reqs");
    let prompt = ch_i64(r, "prompt");
    let cached = ch_i64(r, "cached");
    let completion = ch_i64(r, "completion");
    let errs = ch_i64(r, "errs");
    let ttft_n = ch_i64(r, "ttft_n");
    let mut m = serde_json::Map::new();
    let known = ch_i64(r, "cost_n");
    let known_amount = ch_i64(r, "covered_spend");
    let known_cost = ch_i64(r, "covered_cost_sum");
    let records = ch_i64(r, "financial_records");
    m.insert("financial_records".into(), json!(records));
    m.insert("cost_known_records".into(), json!(known));
    m.insert("cost_known_requests".into(), json!(known));
    m.insert(
        "cost_coverage_bp".into(),
        if records > 0 {
            json!(rate_bp(known, records))
        } else {
            Value::Null
        },
    );
    m.insert("known_amount_micro".into(), json!(known_amount));
    m.insert("known_cost_micro".into(), json!(known_cost));
    m.insert(
        "known_margin_micro".into(),
        if known > 0 {
            json!(known_amount - known_cost)
        } else {
            Value::Null
        },
    );
    m.insert(
        "margin_micro".into(),
        if known == records && records > 0 {
            json!(known_amount - known_cost)
        } else {
            Value::Null
        },
    );
    m.extend(cache_metrics(r, reqs));
    m.insert(
        "suspected_test_requests".into(),
        json!(ch_i64(r, "fixture_reqs")),
    );
    m.insert(
        "suspected_test_tokens".into(),
        json!(ch_i64(r, "fixture_tokens")),
    );
    m.insert("test_detection_basis".into(), json!("fixture_model_name"));

    m.insert("requests".into(), json!(reqs));
    m.insert("errors".into(), json!(errs));
    m.insert("error_rate_bp".into(), json!(rate_bp(errs, reqs)));
    m.insert("prompt_tokens".into(), json!(prompt));
    m.insert("cached_tokens".into(), json!(cached));
    m.insert("completion_tokens".into(), json!(completion));
    m.insert("reasoning_tokens".into(), json!(ch_i64(r, "reasoning")));
    m.insert("tokens".into(), json!(prompt.saturating_add(completion)));
    // 口径与门户 breakdown 一致：命中 token / 输入 token
    m.insert("amount_micro".into(), json!(ch_i64(r, "spend")));
    m.insert("discount_micro".into(), json!(ch_i64(r, "saved")));
    m.insert("upstream_cost_micro".into(), json!(ch_i64(r, "cost")));
    m.extend(super::latency::metrics(
        ch_i64(r, "lat_sum"),
        ch_i64(r, "lat_n"),
        ch_i64(r, "lat_output"),
        reqs,
        ch_i64(r, "lat_observed"),
    ));
    m.extend(super::ttft_average::metrics(
        ch_i64(r, "ttft_s"),
        ttft_n,
        reqs,
        ch_i64(r, "ttft_observed"),
    ));
    m.extend(super::usage_sources::metrics(
        r,
        reqs,
        [Some(prompt), Some(completion)],
        Some(cached),
        ch_i64(r, "read_n"),
    ));
    m.extend(super::token_details::metrics(r, reqs));
    m.extend(super::output_rate::metrics(r, reqs));
    m
}

/// 精简源的行 → 展示字段：与 `pack_metrics` 同名同口径的核心部分，不含任何测量口径
/// （成本覆盖、缓存、延迟、TTFT、用量来源……），避免把"没查"伪装成 0。
fn pack_core_metrics(r: &Value) -> serde_json::Map<String, Value> {
    let reqs = ch_i64(r, "reqs");
    let prompt = ch_i64(r, "prompt");
    let completion = ch_i64(r, "completion");
    let errs = ch_i64(r, "errs");
    let mut m = serde_json::Map::new();
    m.insert("requests".into(), json!(reqs));
    m.insert("errors".into(), json!(errs));
    m.insert("error_rate_bp".into(), json!(rate_bp(errs, reqs)));
    m.insert("prompt_tokens".into(), json!(prompt));
    m.insert("cached_tokens".into(), json!(ch_i64(r, "cached")));
    m.insert("completion_tokens".into(), json!(completion));
    m.insert("reasoning_tokens".into(), json!(ch_i64(r, "reasoning")));
    m.insert("tokens".into(), json!(prompt.saturating_add(completion)));
    m.insert("amount_micro".into(), json!(ch_i64(r, "spend")));
    m.insert("discount_micro".into(), json!(ch_i64(r, "saved")));
    m.insert("upstream_cost_micro".into(), json!(ch_i64(r, "cost")));
    m
}

fn sum_core_rows(rows: &[Value]) -> Value {
    let mut total = json!({});
    for row in rows {
        for key in [
            "reqs",
            "financial_records",
            "prompt",
            "cached",
            "completion",
            "reasoning",
            "spend",
            "saved",
            "cost",
            "errs",
        ] {
            total[key] = json!(ch_i64(&total, key).saturating_add(ch_i64(row, key)));
        }
    }
    total
}

fn cache_metrics(row: &Value, requests: i64) -> serde_json::Map<String, Value> {
    let known = ch_i64(row, "write_n");
    json!({
        "cache_write_known_requests": known,
        "cache_write_tokens": if known == requests { json!(ch_i64(row, "write_sum")) } else { Value::Null },
        // A subtotal, never a claim that unreported requests wrote zero tokens.
        "recorded_cache_write_tokens": if known > 0 || ch_i64(row, "write_sum") > 0 { json!(ch_i64(row, "write_sum")) } else { Value::Null },
        "cache_read_known_requests": ch_i64(row, "read_n"),
    }).as_object().cloned().unwrap_or_default()
}

// ---- 名字回填（PG 点查；结果集 ≤ 数百行） ----

#[derive(Default)]
struct Names {
    users: HashMap<i64, String>,
    /// key → (名字, 前缀, 属主 user_id)
    keys: HashMap<i64, (String, String, i64)>,
    /// channel → (名字, provider)
    channels: HashMap<i64, (String, String)>,
    /// group_code → group_ratio 文本
    groups: HashMap<String, String>,
}

async fn resolve_names(
    state: &AppState,
    users: &[i64],
    keys: &[i64],
    channels: &[i64],
    groups: &[String],
) -> Result<Names, AppError> {
    let mut names = Names::default();
    if !users.is_empty() {
        for r in sqlx::query!(
            r#"SELECT id, username FROM users WHERE id = ANY($1)"#,
            users
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        {
            names.users.insert(r.id, r.username);
        }
    }
    if !keys.is_empty() {
        for r in sqlx::query!(
            r#"SELECT id, name, key_prefix, user_id FROM api_keys WHERE id = ANY($1)"#,
            keys
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        {
            names.keys.insert(r.id, (r.name, r.key_prefix, r.user_id));
        }
    }
    if !channels.is_empty() {
        for r in sqlx::query!(
            r#"SELECT id, name, provider FROM channels WHERE id = ANY($1)"#,
            channels
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        {
            names.channels.insert(r.id, (r.name, r.provider));
        }
    }
    if !groups.is_empty() {
        for r in sqlx::query!(
            r#"SELECT group_code, group_ratio::text AS "ratio!" FROM price_groups
               WHERE group_code = ANY($1)"#,
            groups
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        {
            names.groups.insert(r.group_code, r.ratio);
        }
    }
    Ok(names)
}

/// 过滤条件的名字回填：前端过滤芯片显示"用户 alice"而不是"用户 #42"。
/// 实体已删时名字为 null（芯片退回显示 id），不 404——过滤仍然成立，历史数据还在。
async fn describe_scope(state: &AppState, q: &CubeQuery) -> Result<Value, AppError> {
    let users: Vec<i64> = q.user_id.into_iter().collect();
    let keys: Vec<i64> = q.api_key_id.into_iter().collect();
    let channels: Vec<i64> = q.channel_id.into_iter().collect();
    let groups: Vec<String> = trimmed(q.group.as_deref())
        .map(str::to_owned)
        .into_iter()
        .collect();
    let names = resolve_names(state, &users, &keys, &channels, &groups).await?;
    let mut scope = serde_json::Map::new();
    if let Some(id) = q.user_id {
        scope.insert(
            "user".into(),
            json!({ "id": id, "username": names.users.get(&id) }),
        );
    }
    if let Some(id) = q.api_key_id {
        let k = names.keys.get(&id);
        scope.insert(
            "api_key".into(),
            json!({
                "id": id,
                "name": k.map(|k| k.0.clone()),
                "key_prefix": k.map(|k| k.1.clone()),
                "user_id": k.map(|k| k.2),
            }),
        );
    }
    if let Some(id) = q.channel_id {
        let c = names.channels.get(&id);
        scope.insert(
            "channel".into(),
            json!({ "id": id, "name": c.map(|c| c.0.clone()), "provider": c.map(|c| c.1.clone()) }),
        );
    }
    if let Some(m) = trimmed(q.model.as_deref()) {
        scope.insert("model".into(), json!(m));
    }
    if let Some(g) = trimmed(q.group.as_deref()) {
        scope.insert(
            "group".into(),
            json!({ "code": g, "group_ratio": names.groups.get(g) }),
        );
    }
    Ok(Value::Object(scope))
}

/// GET /admin/stats/trend：过滤后的时间趋势 + 当前 / 上一窗口汇总。
///
/// 单日 / 两日窗口按小时出桶（"今天几点开始烧钱"），更长按天。`total` 与
/// `previous` 是同长度的两段，前端据此给每张 KPI 卡标环比箭头。
pub async fn trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(mut q): Query<CubeQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    q.cached &= super::stats_cache::allowed(&headers);
    q.prepare(&state).await?;
    let days = q.days();
    let scope = q.scope();
    let params = scope.borrow();

    let mut window = q.window_meta.clone();
    window["freshness"] = super::analysis_freshness::read(&state).await?;

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
        let (mut rows, mut previous) = tokio::try_join!(
            super::stats_cache::query(&state, &stacked_sql, &params, q.cached),
            q.previous_totals(&state, &agg, &scope),
        )?;
        let total = q
            .trend_totals(&state, Some(&stack_col), &mut rows, &mut previous)
            .await?;
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

    let (mut series, mut previous) = tokio::try_join!(
        super::stats_cache::query(&state, &series_sql, &params, q.cached),
        q.previous_totals(&state, &agg, &scope),
    )?;
    let total = q
        .trend_totals(&state, None, &mut series, &mut previous)
        .await?;
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
async fn core_trend(
    state: &AppState,
    q: &CubeQuery,
    scope: &Scope,
    bucket_expr: &str,
    granularity: &str,
    window: Value,
) -> Result<Json<Value>, AppError> {
    let sql = format!(
        "SELECT {bucket_expr} AS bucket, {} FROM {} WHERE {}{} GROUP BY bucket ORDER BY bucket",
        super::core_source::AGG,
        q.core_source(false),
        q.window(false),
        scope.clause
    );
    let series = super::stats_cache::query(state, &sql, &scope.borrow(), q.cached).await?;
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
fn fold_stacked(rows: &[Value], limit: usize, rank: &str) -> (Vec<String>, Vec<Value>) {
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
        super::usage_sources::accumulate(cell, r);
        super::token_details::accumulate(cell, r);
        super::output_rate::accumulate(cell, r);
        for field in [
            "reqs",
            "spend",
            "errs",
            "prompt",
            "completion",
            "cached",
            "reasoning",
            "saved",
            "cost",
            "lat_sum",
            "lat_n",
            "lat_observed",
            "lat_output",
            "ttft_s",
            "ttft_n",
            "ttft_observed",
            "read_n",
            "write_sum",
            "write_n",
            "cost_n",
            "covered_spend",
            "covered_cost_sum",
        ] {
            cell[field] = json!(ch_i64(cell, field).saturating_add(ch_i64(r, field)));
        }
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
async fn stack_labels(
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
fn primary_key_column(column: &str) -> bool {
    matches!(
        column,
        "model" | "channel_id" | "user_id" | "api_key_id" | "group_code"
    )
}

fn breakdown_key(by: &str) -> Result<&'static str, AppError> {
    Ok(match by {
        "model" => "model",
        "channel" | "provider" => "channel_id",
        "user" => "user_id",
        "api_key" => "api_key_id",
        "group" => "group_code",
        "model_group" => "toJSONString(tuple(model, group_code))",
        "endpoint" => "endpoint",
        "upstream_endpoint" => "upstream_endpoint",
        "requested_model" => "requested_model",
        "upstream_model" => "upstream_model",
        "node" => "node",
        "request_type" => "request_type",
        "billing_type" => "billing_type",
        _ => return Err(AppError::bad_request().with_param("by")),
    })
}

/// JSONEachRow 把 UInt64 序列化为字符串、UInt32 为数字、LowCardinality(String) 为
/// 字符串——三种形态都归一成字符串键。
fn row_key(r: &Value) -> String {
    match r.get("k") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// 拆分结果的一行：折叠键、累加后的度量、折进来的原始行数。
struct Bucket {
    key: String,
    metrics: serde_json::Map<String, Value>,
    folded: i64,
}

/// Fold additive raw counters first, then recompute all ratios and coverage once.
fn fold_rows(rows: &[Value], fold_key: &dyn Fn(&str) -> String, core: bool) -> Vec<Bucket> {
    let mut raw: Vec<(String, Value, i64)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for r in rows {
        let key = fold_key(&row_key(r));
        let pos = *index.entry(key.clone()).or_insert_with(|| {
            raw.push((key, json!({}), 0));
            raw.len() - 1
        });
        let (_, totals, folded) = &mut raw[pos];
        super::usage_sources::accumulate(totals, r);
        super::token_details::accumulate(totals, r);
        super::output_rate::accumulate(totals, r);
        for field in [
            "reqs",
            "financial_records",
            "prompt",
            "completion",
            "cached",
            "reasoning",
            "spend",
            "saved",
            "cost",
            "errs",
            "lat_sum",
            "lat_n",
            "lat_observed",
            "lat_output",
            "ttft_s",
            "ttft_n",
            "ttft_observed",
            "write_sum",
            "write_n",
            "read_n",
            "cost_n",
            "covered_spend",
            "covered_cost_sum",
        ] {
            totals[field] = json!(ch_i64(totals, field).saturating_add(ch_i64(r, field)));
        }
        *folded += 1;
    }
    raw.into_iter()
        .map(|(key, totals, folded)| Bucket {
            key,
            metrics: if core {
                pack_core_metrics(&totals)
            } else {
                pack_metrics(&totals)
            },
            folded,
        })
        .collect()
}

/// 上期名次：折叠键 → (金额, 名次)。
fn previous_ranks(
    prev: &[Value],
    fold_key: &dyn Fn(&str) -> String,
    metric: BreakdownMetric,
) -> HashMap<String, (i64, usize)> {
    let mut spend: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for r in prev {
        let values = spend.entry(fold_key(&row_key(r))).or_default();
        values.0 = values.0.saturating_add(ch_i64(r, "spend"));
        values.1 = values.1.saturating_add(ch_i64(r, metric.sql_column()));
    }
    let mut ranked: Vec<_> = spend.into_iter().collect();
    ranked.sort_by(|a, b| b.1.1.cmp(&a.1.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .enumerate()
        .map(|(i, (k, v))| (k, (v.0, i + 1)))
        .collect()
}

/// 维度专属的标签列：user 给用户名、api_key 给名字 + 前缀 + 属主、channel 给
/// provider、group 给倍率、provider 给折进来的渠道数。
fn label_bucket(
    m: &mut serde_json::Map<String, Value>,
    by: &str,
    b: &Bucket,
    names: &Names,
    owners: &HashMap<i64, String>,
) {
    let k = &b.key;
    match by {
        "user" => {
            let id = k.parse::<i64>().unwrap_or(0);
            m.insert("user_id".into(), json!(id));
            m.insert("label".into(), json!(names.users.get(&id)));
        }
        "api_key" => {
            let id = k.parse::<i64>().unwrap_or(0);
            let key = names.keys.get(&id);
            m.insert("api_key_id".into(), json!(id));
            m.insert("label".into(), json!(key.map(|k| k.0.clone())));
            m.insert("key_prefix".into(), json!(key.map(|k| k.1.clone())));
            m.insert("user_id".into(), json!(key.map(|k| k.2)));
            m.insert("username".into(), json!(key.and_then(|k| owners.get(&k.2))));
        }
        "channel" => {
            let id = k.parse::<i64>().unwrap_or(0);
            let c = names.channels.get(&id);
            m.insert("channel_id".into(), json!(id));
            m.insert("label".into(), json!(c.map(|c| c.0.clone())));
            m.insert("provider".into(), json!(c.map(|c| c.1.clone())));
        }
        "provider" => {
            m.insert("label".into(), json!(k));
            m.insert("channels".into(), json!(b.folded));
        }
        "group" => {
            m.insert("label".into(), json!(k));
            m.insert("group_ratio".into(), json!(names.groups.get(k)));
        }
        _ => {
            m.insert("label".into(), json!(k));
        }
    }
}

#[derive(Clone, Copy)]
enum BreakdownMetric {
    Amount,
    Requests,
    Tokens,
}

impl BreakdownMetric {
    fn parse(value: Option<&str>) -> Result<Self, AppError> {
        match value.unwrap_or("amount") {
            "amount" => Ok(Self::Amount),
            "requests" => Ok(Self::Requests),
            "tokens" => Ok(Self::Tokens),
            _ => Err(AppError::bad_request().with_param("metric")),
        }
    }

    fn sql_column(self) -> &'static str {
        match self {
            Self::Amount => "spend",
            Self::Requests => "reqs",
            Self::Tokens => "tokens",
        }
    }

    fn field(self) -> &'static str {
        match self {
            Self::Amount => "amount_micro",
            Self::Requests => "requests",
            Self::Tokens => "tokens",
        }
    }

    fn top_buckets(
        self,
        rows: &[Value],
        fold_key: &dyn Fn(&str) -> String,
        core: bool,
        limit: usize,
    ) -> Vec<Bucket> {
        let mut buckets = fold_rows(rows, fold_key, core);
        buckets.sort_by(|a, b| {
            b.metrics[self.field()]
                .as_i64()
                .cmp(&a.metrics[self.field()].as_i64())
                .then_with(|| a.key.cmp(&b.key))
        });
        buckets.truncate(limit);
        buckets
    }
}

/// 当前排行在 SQL 侧排序后截断，provider 则取全渠道折叠再取 Top N。
/// 分母查询和上期排行不截断，分别用于全量占比、上期名次及金额环比。
///
/// 分母与上期排行只读金额 / 请求 / Token，只要精简源能给出同样的值就用它——这两条
/// 在完整源上各要 ~5s 的规划时间。`core` 表示当前排行也只要核心指标。
/// 上期排行在调用方不要环比（`compare=false`）时为 `None`。
fn breakdown_sql(
    q: &CubeQuery,
    scope: &Scope,
    key_col: &str,
    fold_provider: bool,
    limit: usize,
    metric: BreakdownMetric,
    core: bool,
) -> (String, String, Option<String>) {
    let order = metric.sql_column();
    let sql_limit = if fold_provider {
        String::new()
    } else {
        format!(" LIMIT {limit}")
    };
    // 分母与上期排行只读 3 列：能走精简源就走（`core` 只决定当前排行是否也精简）。
    let source = |previous: bool, key: Option<&str>| {
        if q.core_source_ok(key, previous) {
            q.core_source(previous)
        } else {
            q.source(previous)
        }
    };
    let (cur_agg, cur_source) = if core {
        (super::core_source::AGG.to_owned(), q.core_source(false))
    } else {
        (aggregate_metrics(), q.source(false))
    };
    let cur = format!(
        "SELECT {key_col} AS k, {cur_agg}, sum(prompt_tokens) + sum(completion_tokens) AS tokens \
         FROM {cur_source} WHERE {}{} \
         GROUP BY k ORDER BY {order} DESC, k{sql_limit}",
        q.window(false),
        scope.clause
    );
    let total = format!(
        "SELECT sum(amount) AS spend, sum(requests) AS reqs, \
         sum(prompt_tokens) + sum(completion_tokens) AS tokens \
         FROM {} WHERE {}{}",
        source(false, None),
        q.window(false),
        scope.clause
    );
    let prev = q.compares().then(|| {
        format!(
            "SELECT {key_col} AS k, sum(amount) AS spend, sum(requests) AS reqs, \
             sum(prompt_tokens) + sum(completion_tokens) AS tokens \
             FROM {} WHERE {}{} GROUP BY k ORDER BY {order} DESC, k",
            source(true, Some(key_col)),
            q.window(true),
            scope.clause
        )
    });
    (cur, total, prev)
}

/// 排名按所选指标变化；各类占比与金额环比仍保留各自的单位和全量分母。
fn ranked_metrics(
    b: &Bucket,
    rank: usize,
    prev: Option<&(i64, usize)>,
    total_spend: i64,
    total_reqs: i64,
    total_tokens: i64,
    previous_known: bool,
) -> serde_json::Map<String, Value> {
    let mut m = b.metrics.clone();
    let spend = m["amount_micro"].as_i64().unwrap_or(0);
    let reqs = m["requests"].as_i64().unwrap_or(0);
    m.insert("key".into(), json!(b.key));
    m.insert("rank".into(), json!(rank));
    m.insert("previous_rank".into(), json!(prev.map(|p| p.1)));
    // 没查上一窗口（compare=false / fields=core）时是"未知"，不是金额 0。
    m.insert(
        "previous_amount_micro".into(),
        if previous_known {
            json!(prev.map_or(0, |p| p.0))
        } else {
            Value::Null
        },
    );
    // 环比（基点）：上期为 0 时无意义给 null，前端显示"新"
    m.insert(
        "delta_bp".into(),
        json!(
            prev.map(|p| p.0)
                .filter(|p| *p > 0)
                .map(|p| rate_bp(spend.saturating_sub(p), p))
        ),
    );
    m.insert("share_bp".into(), json!(rate_bp(spend, total_spend)));
    m.insert("request_share_bp".into(), json!(rate_bp(reqs, total_reqs)));
    let tokens = m["tokens"].as_i64().unwrap_or(0);
    m.insert(
        "token_share_bp".into(),
        json!(rate_bp(tokens, total_tokens)),
    );
    m
}

/// GET /admin/stats/breakdown?by=：过滤后按另一维度拆分（Sub2API UserBreakdown 的
/// 泛化：它只答"谁在用这个模型 / 分组"，这里任一维度都能当拆分轴）。
///
/// 每行带占比、环比（同长度上一窗口的金额变化，基点）与上期名次——new-api
/// 排行榜页的 share / growth / previous_rank 三个字段；`provider` 维度由渠道行
/// 按 PG 里的 provider 折叠得到（provider 不进立方体键，见 database.md §3.2）。
pub async fn breakdown(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(mut q): Query<CubeQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let metric = BreakdownMetric::parse(q.metric.as_deref())?;
    q.cached &= super::stats_cache::allowed(&headers);
    q.prepare(&state).await?;
    let by = q.by.as_deref().unwrap_or("model");
    let key_col = if by == "model" {
        q.model_column()?
    } else {
        breakdown_key(by)?
    };
    let fold_provider = by == "provider";
    let limit = q.limit.unwrap_or(20).clamp(1, 100) as usize;
    let scope = q.scope();
    let params = scope.borrow();

    // 当前排行只要核心指标且精简源能给出同样的值时，不恢复缓存口径（核心指标不含缓存）。
    let core = q.core_requested()? && q.core_source_ok(Some(key_col), false);
    let (cur_sql, total_sql, prev_sql) =
        breakdown_sql(&q, &scope, key_col, fold_provider, limit, metric, core);
    let (mut cur, total, prev) = tokio::try_join!(
        super::stats_cache::query(&state, &cur_sql, &params, q.cached),
        super::stats_cache::query(&state, &total_sql, &params, q.cached),
        async {
            match &prev_sql {
                Some(sql) => super::stats_cache::query(&state, sql, &params, q.cached).await,
                None => Ok(Vec::new()),
            }
        },
    )?;

    if !core {
        q.recover_cache(&state, false, None, Some(key_col), &mut cur)
            .await?;
    }

    // 名字回填：渠道维度还要 provider（折叠依据）
    let int_keys: Vec<i64> = cur
        .iter()
        .chain(prev.iter())
        .filter_map(|r| row_key(r).parse::<i64>().ok())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let str_keys: Vec<String> = cur.iter().map(row_key).collect();
    let names = match by {
        "user" => resolve_names(&state, &int_keys, &[], &[], &[]).await?,
        "api_key" => resolve_names(&state, &[], &int_keys, &[], &[]).await?,
        "channel" | "provider" => resolve_names(&state, &[], &[], &int_keys, &[]).await?,
        "group" => resolve_names(&state, &[], &[], &[], &str_keys).await?,
        _ => Names::default(),
    };
    // api_key 维度再补属主用户名（"谁的哪把 key"）
    let owners = if by == "api_key" {
        let ids: Vec<i64> = names.keys.values().map(|k| k.2).collect();
        resolve_names(&state, &ids, &[], &[], &[]).await?.users
    } else {
        HashMap::new()
    };

    // 折叠函数：渠道 id → provider 名；其余维度恒等
    let fold_key = |raw: &str| -> String {
        if fold_provider {
            raw.parse::<i64>()
                .ok()
                .and_then(|id| names.channels.get(&id))
                .map_or_else(|| "unknown".to_owned(), |c| c.1.clone())
        } else {
            raw.to_owned()
        }
    };
    let prev_ranks = previous_ranks(&prev, &fold_key, metric);
    let mut buckets = metric.top_buckets(&cur, &fold_key, core, limit);
    if fold_provider && !core {
        q.recover_provider_cache(&state, &mut buckets, &cur, &names)
            .await?;
    }

    let total_spend = total.first().map_or(0, |r| ch_i64(r, "spend"));
    let total_reqs = total.first().map_or(0, |r| ch_i64(r, "reqs"));
    let total_tokens = total.first().map_or(0, |r| ch_i64(r, "tokens"));
    let totals = (total_spend, total_reqs, total_tokens);
    let data = ranked_rows(
        &buckets,
        totals,
        &prev_ranks,
        q.compares(),
        by,
        &names,
        &owners,
    );

    Ok(Json(json!({
        "days": q.days(),
        "window": q.window_meta,
        "by": by,
        "scope": describe_scope(&state, &q).await?,
        "total_amount_micro": total_spend,
        "total_requests": total_reqs,
        "total_tokens": total_tokens,
        "fields": if core { "core" } else { "all" },
        "data": data,
    })))
}

/// 排行桶 → 响应行：名次、占比（全量分母）、上期信息与维度专属标签。
fn ranked_rows(
    buckets: &[Bucket],
    totals: (i64, i64, i64),
    prev_ranks: &HashMap<String, (i64, usize)>,
    previous_known: bool,
    by: &str,
    names: &Names,
    owners: &HashMap<i64, String>,
) -> Vec<Value> {
    let (spend, reqs, tokens) = totals;
    buckets
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut m = ranked_metrics(
                b,
                i + 1,
                prev_ranks.get(&b.key),
                spend,
                reqs,
                tokens,
                previous_known,
            );
            label_bucket(&mut m, by, b, names, owners);
            Value::Object(m)
        })
        .collect()
}

/// 流向阶段按调用路径排列；管理员可以隐藏中间阶段，链接从原始组合重新汇总。
const FLOW_STAGES: [&str; 6] = ["user", "node", "api_key", "group", "model", "channel"];
const FLOW_OTHER: &str = "__other";
/// 五维组合取消耗最高的前 N 个；超出即 `truncated`，`coverage_bp` 标注覆盖比例。
const FLOW_ROWS: usize = 5_000;

/// 组合行在某阶段上的节点键。
fn flow_stage_key(r: &Value, stage_name: &str) -> String {
    match stage_name {
        "user" => ch_i64(r, "user_id").to_string(),
        "api_key" => ch_i64(r, "api_key_id").to_string(),
        "group" => ch_str(r, "group_code").to_owned(),
        "node" => ch_str(r, "node").to_owned(),
        "model" => ch_str(r, "flow_model").to_owned(),
        _ => ch_i64(r, "channel_id").to_string(),
    }
}

/// 折叠后的桑基图：节点值、相邻阶段链接、每阶段保留下来的节点键。
struct FlowGraph {
    nodes: BTreeMap<String, i64>,
    links: BTreeMap<(String, String), i64>,
    keep: Vec<HashSet<String>>,
    covered: i64,
}

impl FlowGraph {
    /// 每阶段取 Top N 节点（度量降序、键升序稳定），其余折进 `__other`；
    /// 节点 id 形如 `stage:key`，链接只在相邻阶段之间。
    fn build(combos: &[Value], per_stage: usize, order_col: &str, stages: &[&str]) -> Self {
        let mut stage_totals: Vec<HashMap<String, i64>> = vec![HashMap::new(); stages.len()];
        let mut covered = 0_i64;
        for r in combos {
            let v = ch_i64(r, order_col);
            covered = covered.saturating_add(v);
            for (i, stage_name) in stages.iter().enumerate() {
                *stage_totals[i]
                    .entry(flow_stage_key(r, stage_name))
                    .or_default() += v;
            }
        }
        let keep: Vec<HashSet<String>> = stage_totals
            .iter()
            .map(|totals| {
                let mut ranked: Vec<(&String, &i64)> = totals.iter().collect();
                ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                ranked
                    .into_iter()
                    .take(per_stage)
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .collect();
        let node_id = |stage_idx: usize, key: &str| -> String {
            let k = if keep[stage_idx].contains(key) {
                key
            } else {
                FLOW_OTHER
            };
            format!("{}:{}", stages[stage_idx], k)
        };

        let mut nodes: BTreeMap<String, i64> = BTreeMap::new();
        let mut links: BTreeMap<(String, String), i64> = BTreeMap::new();
        for r in combos {
            let v = ch_i64(r, order_col);
            let ids: Vec<String> = stages
                .iter()
                .enumerate()
                .map(|(i, s)| node_id(i, &flow_stage_key(r, s)))
                .collect();
            for id in &ids {
                *nodes.entry(id.clone()).or_default() += v;
            }
            for pair in ids.windows(2) {
                *links.entry((pair[0].clone(), pair[1].clone())).or_default() += v;
            }
        }
        Self {
            nodes,
            links,
            keep,
            covered,
        }
    }

    fn kept_ids(&self, stage_idx: usize) -> Vec<i64> {
        self.keep[stage_idx]
            .iter()
            .filter_map(|k| k.parse::<i64>().ok())
            .collect()
    }
}

/// Flow identities include lifecycle/context so absent names never masquerade as readable labels.
/// Read only display names and safe key prefixes; credentials never enter this response.
async fn flow_identities(
    state: &AppState,
    users: &[i64],
    keys: &[i64],
    channels: &[i64],
) -> Result<HashMap<String, Value>, AppError> {
    let mut identities = HashMap::new();
    if !users.is_empty() {
        let rows: Vec<(i64, String, bool)> = sqlx::query_as(
            "SELECT id, username, deleted_at IS NOT NULL FROM users WHERE id = ANY($1)",
        )
        .bind(users)
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        for (id, name, deleted) in rows {
            identities.insert(
                format!("user:{id}"),
                json!({"label": name, "entity_status": if deleted { "deleted" } else { "active" }}),
            );
        }
    }
    if !keys.is_empty() {
        let rows: Vec<(i64, String, String, Option<String>, bool)> = sqlx::query_as("SELECT k.id, k.name, k.key_prefix, u.username, k.deleted_at IS NOT NULL FROM api_keys k LEFT JOIN users u ON u.id = k.user_id WHERE k.id = ANY($1)")
            .bind(keys).fetch_all(&state.pg).await.map_err(okapi_store::StoreError::from)?;
        for (id, name, prefix, owner, deleted) in rows {
            identities.insert(format!("api_key:{id}"), json!({"label": name, "key_prefix": prefix, "owner_name": owner, "entity_status": if deleted { "deleted" } else { "active" }}));
        }
    }
    if !channels.is_empty() {
        let rows: Vec<(i64, String, String, bool)> = sqlx::query_as(
            "SELECT id, name, provider, deleted_at IS NOT NULL FROM channels WHERE id = ANY($1)",
        )
        .bind(channels)
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        for (id, name, provider, deleted) in rows {
            identities.insert(format!("channel:{id}"), json!({"label": name, "provider": provider, "entity_status": if deleted { "deleted" } else { "active" }}));
        }
    }
    Ok(identities)
}

/// GET /admin/stats/flow：桑基图数据（钱 / 请求 / token 从谁、经哪把 key、
/// 哪个分组、哪个模型、流到哪条渠道）。
///
/// 一条 GROUP BY 五维的查询取消耗最高的前 `FLOW_ROWS` 个组合；每阶段各取
/// Top N 节点、其余折进"其他"。`coverage_bp` 标注所取组合覆盖了窗口内多大比例的
/// 度量——组合数超过上限时图是"头部的流向"而非全量，得让人知道。
pub async fn flow(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(mut q): Query<CubeQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    q.cached &= super::stats_cache::allowed(&headers);
    q.prepare(&state).await?;
    let per_stage = q.limit.unwrap_or(6).clamp(1, 20) as usize;
    let metric = match q.metric.as_deref().unwrap_or("amount") {
        "amount" => "amount",
        "requests" => "requests",
        "tokens" => "tokens",
        _ => return Err(AppError::bad_request().with_param("metric")),
    };
    let order_col = match metric {
        "requests" => "reqs",
        "tokens" => "toks",
        _ => "spend",
    };
    let agg = aggregate_metrics();
    let current_source = q.source(false);
    let scope = q.scope();
    let params = scope.borrow();

    let model_col = q.model_column()?;
    let selected = parse_choices("stages", q.stages.as_deref())?;
    if q.stages.is_some()
        && (selected.len() < 2
            || selected.iter().any(|s| !FLOW_STAGES.contains(&s.as_str()))
            || selected.iter().collect::<HashSet<_>>().len() != selected.len())
    {
        return Err(AppError::bad_request().with_param("stages"));
    }
    let stages: Vec<&str> = FLOW_STAGES
        .iter()
        .copied()
        .filter(|s| selected.is_empty() || selected.iter().any(|v| v == s))
        .collect();
    let combo_sql = format!(
        "SELECT user_id, api_key_id, group_code, {model_col} AS flow_model, channel_id, node, \
                sum(requests) AS reqs, sum(amount) AS spend, \
                sum(prompt_tokens) + sum(completion_tokens) AS toks \
         FROM {current_source} WHERE {}{} \
         GROUP BY user_id, api_key_id, group_code, flow_model, channel_id, node \
         ORDER BY {order_col} DESC LIMIT {FLOW_ROWS}",
        q.window(false),
        scope.clause
    );
    let total_sql = format!(
        "SELECT {agg}, sum(prompt_tokens) + sum(completion_tokens) AS toks \
         FROM {current_source} WHERE {}{}",
        q.window(false),
        scope.clause
    );
    let combos = ch.query_with_params(&combo_sql, &params).await?;
    let mut total = ch.query_with_params(&total_sql, &params).await?;
    q.recover_cache(&state, false, None, None, &mut total)
        .await?;
    let total_metric = total.first().map_or(0, |r| ch_i64(r, order_col));

    let graph = FlowGraph::build(&combos, per_stage, order_col, &stages);
    let ids = |name| {
        stages
            .iter()
            .position(|s| *s == name)
            .map_or_else(Vec::new, |i| graph.kept_ids(i))
    };
    let identities =
        flow_identities(&state, &ids("user"), &ids("api_key"), &ids("channel")).await?;

    let nodes: Vec<Value> = graph.nodes.iter().map(|(id, v)| {
        let (stage_name, key) = id.split_once(':').unwrap_or((id, ""));
        let entity = matches!(stage_name, "user" | "api_key" | "channel");
        let other = key == FLOW_OTHER;
        let mut node = json!({
            "id": id, "stage": stage_name, "key": key,
            "label": if entity || other { Value::Null } else { json!(key) },
            "entity_status": if other || !entity { Value::Null } else if key == "0" { json!("unassigned") } else { json!("missing") },
            "other": other, "value": v,
        });
        if let Some(fields) = identities.get(id).and_then(Value::as_object) {
            node.as_object_mut().unwrap().extend(fields.clone());
        }
        node
    }).collect();
    let links: Vec<Value> = graph
        .links
        .iter()
        .map(|((s, t), v)| json!({ "source": s, "target": t, "value": v }))
        .collect();

    Ok(Json(json!({
        "days": q.days(),
        "window": q.window_meta,
        "metric": metric,
        "scope": describe_scope(&state, &q).await?,
        "stages": stages,
        "total": total_metric,
        "metrics": total.first().map(pack_metrics),
        "coverage_bp": rate_bp(graph.covered, total_metric),
        "truncated": combos.len() >= FLOW_ROWS,
        "nodes": nodes,
        "links": links,
    })))
}

/// 渠道 key 六态各多少把（§3.4 状态机：1 可用 / 2 冷却 / 3 限速 / 4 额度耗尽 / 5 封禁 / 6 凭证无效）。
async fn channel_key_status_counts(pg: &sqlx::PgPool) -> Result<Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT k.status AS "status!", count(*) AS "n!"
           FROM channel_keys k JOIN channels c ON c.id = k.channel_id
           WHERE c.deleted_at IS NULL GROUP BY k.status"#
    )
    .fetch_all(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let n = |s: i16| rows.iter().find(|r| r.status == s).map_or(0, |r| r.n);
    Ok(json!({
        "active": n(1),
        "cooling": n(2),
        "rate_limited": n(3),
        "quota_exhausted": n(4),
        "banned": n(5),
        "invalid": n(6),
    }))
}

/// GET /admin/stats/inventory：站点规模（Sub2API DashboardStats 的实体计数区 +
/// 老 ok-api Overview 的 channels total/active/healthy）。
///
/// 全部 PG 计数、不碰 CH——最小部署（无 CH）的落地页此前除了实时条什么数字
/// 都没有。"多少用户、几把 key、几条渠道健康"是站长打开后台的第一眼。
pub async fn inventory(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let pg = &state.pg;

    let users = sqlx::query!(
        r#"SELECT count(*) AS "total!",
                  count(*) FILTER (WHERE status = 1) AS "active!",
                  count(*) FILTER (WHERE created_at >= date_trunc('day', now())) AS "new_today!",
                  count(*) FILTER (WHERE created_at >= now() - interval '7 days') AS "new_7d!"
           FROM users WHERE deleted_at IS NULL AND kind = 'user'"#
    )
    .fetch_one(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let keys = sqlx::query!(
        r#"SELECT count(*) AS "total!",
                  count(*) FILTER (WHERE status = 1 AND (expires_at IS NULL OR expires_at > now())) AS "active!",
                  count(*) FILTER (WHERE last_used_at >= now() - interval '7 days') AS "used_7d!"
           FROM api_keys WHERE deleted_at IS NULL"#
    )
    .fetch_one(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    // 渠道健康按"实际能不能打"分三档：启用且至少一把 key 可用 / 启用但零可用 key /
    // 停用（手动 2 + 自动 3）。渠道级 status 绿着、key 全在冷却的渠道在列表页
    // 已按 §11.12 显示为"无可用 key"，这里同口径。
    let channels = sqlx::query!(
        r#"SELECT count(*) AS "total!",
                  count(*) FILTER (WHERE c.status = 1 AND EXISTS (
                      SELECT 1 FROM channel_keys k WHERE k.channel_id = c.id AND k.status = 1
                  )) AS "healthy!",
                  count(*) FILTER (WHERE c.status = 1 AND NOT EXISTS (
                      SELECT 1 FROM channel_keys k WHERE k.channel_id = c.id AND k.status = 1
                  )) AS "no_key!",
                  count(*) FILTER (WHERE c.status = 3) AS "auto_disabled!",
                  count(*) FILTER (WHERE c.status = 2) AS "disabled!",
                  count(*) FILTER (WHERE NOT EXISTS (
                      SELECT 1 FROM pool_channels pc WHERE pc.channel_id = c.id
                  )) AS "orphan!"
           FROM channels c WHERE c.deleted_at IS NULL"#
    )
    .fetch_one(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let channel_keys = channel_key_status_counts(pg).await?;

    let models = sqlx::query!(
        r#"SELECT count(*) AS "total!",
                  count(p.model_id) AS "priced!",
                  count(*) FILTER (WHERE EXISTS (
                      SELECT 1 FROM channels c
                      WHERE c.deleted_at IS NULL AND c.status = 1 AND c.models ? m.model_name
                  )) AS "served!"
           FROM models m LEFT JOIN model_pricing p ON p.model_id = m.id
           WHERE m.status = 1"#
    )
    .fetch_one(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let groups = sqlx::query!(r#"SELECT count(*) AS "n!" FROM price_groups"#)
        .fetch_one(pg)
        .await
        .map_err(okapi_store::StoreError::from)?;

    Ok(Json(json!({
        "users": {
            "total": users.total,
            "active": users.active,
            "new_today": users.new_today,
            "new_7d": users.new_7d,
        },
        "api_keys": {
            "total": keys.total,
            "active": keys.active,
            "used_7d": keys.used_7d,
        },
        "channels": {
            "total": channels.total,
            "healthy": channels.healthy,
            "no_key": channels.no_key,
            "auto_disabled": channels.auto_disabled,
            "disabled": channels.disabled,
            // 不在任何池里的渠道对谁都不可达（§11.14 唯一规则的直接后果），列表页同样标出
            "orphan": channels.orphan,
        },
        "channel_keys": channel_keys,
        "models": {
            "total": models.total,
            "priced": models.priced,
            "served": models.served,
        },
        "groups": groups.n,
    })))
}

#[derive(Deserialize)]
pub struct EntityUsageQuery {
    /// user | api_key
    pub kind: String,
    /// 逗号分隔 id（≤100，列表页一页的量）。
    pub ids: String,
    #[serde(default)]
    pub days: Option<u32>,
}

/// GET /admin/stats/entity-usage：列表页行内用量（今日 / 窗口消费、请求数、最近活跃日）。
///
/// Sub2API 的用户列表与 key 列表每行直接显示 today / total 消费（batch 端点按可见
/// id 取），比"点进去才看得到"少一次跳转；这里同法：前端把当页 id 一次带来，
/// 单维 MV（mv_user_day / mv_apikey_day）按主键前缀点查，与请求量无关。
pub async fn entity_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EntityUsageQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let (table, col) = match q.kind.as_str() {
        "user" => ("mv_user_day", "user_id"),
        "api_key" => ("mv_apikey_day", "api_key_id"),
        _ => return Err(AppError::bad_request().with_param("kind")),
    };
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let ids: Vec<i64> = q
        .ids
        .split(',')
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .take(100)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(Json(json!({ "days": days, "data": {} })));
    }
    let id_list = ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let predicate = format!(
        "{col} IN ({id_list}) AND day >= today() - {} AND day <= today()",
        days - 1
    );
    // 今日与窗口分两组聚合，Rust 侧合并——比 sumMergeIf 组合子的可移植性更稳
    let sql = format!(
        "SELECT {col} AS k, day = today() AS is_today, \
                sumMerge(amount) AS spend, countMerge(requests) AS reqs, max(day) AS last_day \
         FROM {table} WHERE {predicate} \
         GROUP BY k, is_today"
    );
    let rows = ch.query_json_each_row(&sql).await?;

    let mut data: BTreeMap<String, Value> = BTreeMap::new();
    for r in &rows {
        let k = ch_i64(r, "k").to_string();
        let is_today = ch_i64(r, "is_today") == 1;
        let spend = ch_i64(r, "spend");
        let reqs = ch_i64(r, "reqs");
        let last_day = ch_str(r, "last_day").to_owned();
        let entry = data.entry(k).or_insert_with(|| {
            json!({ "today_micro": 0, "window_micro": 0, "requests": 0, "last_day": Value::Null })
        });
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        let bump = |obj: &mut serde_json::Map<String, Value>, field: &str, v: i64| {
            let cur = obj.get(field).and_then(Value::as_i64).unwrap_or(0);
            obj.insert(field.to_owned(), json!(cur.saturating_add(v)));
        };
        if is_today {
            bump(obj, "today_micro", spend);
        }
        bump(obj, "window_micro", spend);
        bump(obj, "requests", reqs);
        let prev_day = obj.get("last_day").and_then(Value::as_str).unwrap_or("");
        if last_day.as_str() > prev_day {
            obj.insert("last_day".into(), json!(last_day));
        }
    }
    enrich_entities(ch, &mut data, col, table, &predicate).await?;
    Ok(Json(json!({ "days": days, "data": data })))
}

async fn enrich_entities(
    ch: &ChClient,
    data: &mut BTreeMap<String, Value>,
    column: &str,
    table: &str,
    predicate: &str,
) -> Result<(), AppError> {
    let mut rows: Vec<Value> = data
        .iter()
        .map(|(id, value)| {
            let mut row = value.clone();
            row[column] = json!(id);
            row
        })
        .collect();
    super::usage_sources::enrich(ch, column, table, predicate, &mut rows).await?;
    super::output_rate::enrich_entities(ch, &mut rows, column, table, predicate).await?;
    for mut row in rows {
        let id = row[column].as_str().unwrap_or_default().to_owned();
        if let Some(fields) = row.as_object_mut() {
            fields.remove(column);
        }
        data.insert(id, row);
    }
    Ok(())
}

fn parse_choices(name: &str, value: Option<&str>) -> Result<Vec<String>, AppError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values: Vec<String> =
        serde_json::from_str(value).map_err(|_| AppError::bad_request().with_param(name))?;
    if values.len() > 8 || values.iter().any(|v| v.trim().is_empty() || v.len() > 256) {
        return Err(AppError::bad_request().with_param(name));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::measurement_coverage::Coverage;

    fn query() -> CubeQuery {
        CubeQuery::default()
    }

    #[test]
    fn core_source_is_only_used_when_it_matches_the_full_source() {
        let q = query();
        for column in [None, Some("model"), Some("channel_id"), Some("user_id")] {
            assert!(q.core_source_ok(column, false), "{column:?}");
        }
        for column in ["endpoint", "node", "toJSONString(tuple(model, group_code))"] {
            assert!(!q.core_source_ok(Some(column), false), "{column}");
        }
        let mut detail = query();
        detail.stream = Some(true);
        assert!(!detail.core_source_ok(None, false));
        let mut requested = query();
        requested.model_source = Some("requested".to_owned());
        assert!(!requested.core_source_ok(None, false));
        assert!(!requested.core_source_ok(Some("model"), false));
    }

    #[test]
    fn historical_character_units_force_the_full_source_per_window() {
        let mut q = query();
        q.coverage = Coverage {
            historical_units: true,
            ..Coverage::default()
        };
        assert!(!q.core_source_ok(None, false));
        // The previous window is judged on its own coverage.
        assert!(q.core_source_ok(None, true));
    }

    #[test]
    fn fields_and_compare_parameters_are_validated_and_default_to_everything() {
        assert!(!query().core_requested().unwrap());
        assert!(query().compares());
        for (value, core) in [("core", true), ("all", false), (" core ", true)] {
            let mut q = query();
            q.fields = Some(value.to_owned());
            assert_eq!(q.core_requested().unwrap(), core, "{value}");
        }
        let mut q = query();
        q.fields = Some("everything".to_owned());
        assert!(q.core_requested().is_err());
        q.compare = Some(false);
        assert!(!q.compares());
    }

    #[test]
    fn core_queries_never_look_at_the_previous_window() {
        let mut q = query();
        q.fields = Some("core".to_owned());
        assert!(!q.compares());
        q.compare = Some(true);
        assert!(!q.compares(), "fields=core implies compare=false");
        let mut full = query();
        full.compare = Some(true);
        assert!(full.compares());
    }

    #[test]
    fn core_candidate_follows_the_static_conditions_of_the_lean_source() {
        let mut q = query();
        assert!(!q.core_candidate(), "without fields=core nothing is lean");
        q.fields = Some("core".to_owned());
        assert!(q.core_candidate());
        for by in ["model", "channel", "provider", "user", "api_key", "group"] {
            q.by = Some(by.to_owned());
            assert!(q.core_candidate(), "{by}");
        }
        for by in ["endpoint", "node", "model_group", "requested_model"] {
            q.by = Some(by.to_owned());
            assert!(!q.core_candidate(), "{by}");
        }
        q.by = None;
        let mut detail = q_core();
        detail.stream = Some(true);
        assert!(!detail.core_candidate());
        let mut stacked = q_core();
        stacked.stack = Some("model".to_owned());
        assert!(!stacked.core_candidate());
        let mut requested = q_core();
        requested.model_source = Some("requested".to_owned());
        assert!(!requested.core_candidate());
    }

    fn q_core() -> CubeQuery {
        let mut q = query();
        q.fields = Some("core".to_owned());
        q
    }

    #[test]
    fn previous_amount_is_null_when_the_previous_window_was_not_queried() {
        let bucket = Bucket {
            key: "m".to_owned(),
            metrics: pack_core_metrics(
                &json!({"reqs": 2, "spend": 900, "prompt": 1, "completion": 1}),
            ),
            folded: 1,
        };
        let unknown = ranked_metrics(&bucket, 1, None, 900, 2, 2, false);
        assert!(unknown["previous_amount_micro"].is_null());
        assert!(unknown["previous_rank"].is_null() && unknown["delta_bp"].is_null());
        // Queried but this key had no previous usage: a real zero, rank still unknown.
        let none_before = ranked_metrics(&bucket, 1, None, 900, 2, 2, true);
        assert_eq!(none_before["previous_amount_micro"], 0);
        assert!(none_before["previous_rank"].is_null());
    }

    #[test]
    fn core_rows_pack_and_sum_without_inventing_measurement_fields() {
        let rows = [
            json!({"reqs": "2", "prompt": "30", "cached": "5", "completion": "70", "reasoning": "1", "spend": "900", "saved": "10", "cost": "400", "errs": "1"}),
            json!({"reqs": 3, "prompt": 20, "cached": 0, "completion": 30, "reasoning": 0, "spend": 100, "saved": 0, "cost": 50, "errs": 0}),
        ];
        let total = pack_core_metrics(&sum_core_rows(&rows));
        assert_eq!(total["requests"], 5);
        assert_eq!(total["errors"], 1);
        assert_eq!(total["error_rate_bp"], 2_000);
        assert_eq!(total["tokens"], 150);
        assert_eq!(total["amount_micro"], 1_000);
        assert_eq!(total["upstream_cost_micro"], 450);
        for unmeasured in [
            "avg_latency_ms",
            "avg_ttft_ms",
            "cost_coverage_bp",
            "cache_hit_bp",
            "token_provenance",
        ] {
            assert!(!total.contains_key(unmeasured), "{unmeasured}");
        }
    }
}
