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
//!
//! 本文件放查询参数（`CubeQuery` → 过滤条件 `Scope`）、度量列与名称解析；四个端点与它们的
//! 折叠逻辑在子模块（trend / breakdown / flow / inventory），度量累加与打包在 metrics。

use axum::http::StatusCode;
use chrono::Days;
use okapi_api::codes;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

mod breakdown;
mod flow;
mod inventory;
mod metrics;
mod trend;

pub use self::breakdown::breakdown;
use self::breakdown::{breakdown_key, row_key};
use self::flow::FLOW_OTHER;
pub use self::flow::flow;
use self::inventory::parse_choices;
pub use self::inventory::{EntityUsageQuery, entity_usage, inventory};
use self::metrics::{
    accumulate_core_metrics, accumulate_original, aggregate_metrics, pack_core_metrics,
    pack_metrics, sum_core_rows, sum_metric_rows,
};
use self::trend::primary_key_column;
pub use self::trend::trend;

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
        // 负数 id 在拼条件时会被跳过：界面上显示「已限定某用户」，数字却是全站的。0 是「未归属」，合法
        for (param, value) in [
            ("user_id", self.user_id),
            ("api_key_id", self.api_key_id),
            ("channel_id", self.channel_id),
        ] {
            if value.is_some_and(|v| v < 0) {
                return Err(AppError::bad_request().with_param(param));
            }
        }
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
        // 当期与上期各自判断有没有历史字符口径（老语音计费数据要做 prompt_tokens 校正）。
        let primary = self.primary_scope();
        let params = primary.borrow();
        let predicate = format!("{}{}", self.window(false), primary.clause);
        self.coverage =
            super::measurement_coverage::Coverage::read(state, &predicate, &params).await?;
        if self.compares() {
            let predicate = format!("{}{}", self.window(true), primary.clause);
            self.previous_coverage =
                super::measurement_coverage::Coverage::read(state, &predicate, &params).await?;
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
        self.primary_scope().clause
    }

    fn primary_scope(&self) -> Scope {
        // Trim all primary dimensions before coverage/recovery. String values
        // remain bound; missing detailed dimensions still belong to the remainder.
        let billed = self.model_column().is_ok_and(|col| col == "model");
        Self {
            user_id: self.user_id,
            api_key_id: self.api_key_id,
            channel_id: self.channel_id,
            model: billed.then(|| self.model.clone()).flatten(),
            models: billed.then(|| self.models.clone()).flatten(),
            group: self.group.clone(),
            groups: self.groups.clone(),
            ..Self::default()
        }
        .scope()
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

#[cfg(test)]
mod tests {
    use super::breakdown::{Bucket, fold_rows, ranked_metrics};
    use super::trend::fold_stacked;
    use super::*;
    use crate::measurement_coverage::Coverage;

    #[test]
    fn folding_breakdown_rows_keeps_every_trend_counter() {
        let rows = [
            json!({"k": "a", "reqs": 3, "fixture_reqs": 2, "fixture_tokens": 40}),
            json!({"k": "b", "reqs": 1, "fixture_reqs": 1, "fixture_tokens": 5}),
        ];
        let buckets = fold_rows(&rows, &|_| "all".to_owned(), false);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].metrics["suspected_test_requests"], 3);
        assert_eq!(buckets[0].metrics["suspected_test_tokens"], 45);
    }

    fn query() -> CubeQuery {
        CubeQuery::default()
    }

    #[test]
    fn signed_original_amount_is_exact_in_full_and_core_metrics() {
        for (amount, discount, original) in [
            (43200, -21600, Some(21600)),
            (10800, 10800, Some(21600)),
            (-43200, 21600, Some(-21600)),
            (0, 0, Some(0)),
            (i64::MAX, 1, None),
            (i64::MIN, -1, None),
        ] {
            let row = json!({"spend":amount,"saved":discount});
            for packed in [pack_metrics(&row), pack_core_metrics(&row)] {
                assert_eq!(packed["amount_micro"], amount);
                assert_eq!(packed["discount_micro"], discount);
                assert_eq!(packed["original_amount_micro"], json!(original));
            }
        }
    }

    #[test]
    fn original_amount_keeps_unknown_integer_inputs_distinct_from_zero() {
        for (row, original) in [
            (json!({}), json!(0)),
            (json!({"spend":"43200","saved":"-21600"}), json!(21600)),
            (json!({"spend":0,"saved":0}), json!(0)),
            (json!({"spend":null,"saved":0}), Value::Null),
            (json!({"spend":false,"saved":0}), Value::Null),
            (json!({"spend":"invalid","saved":0}), Value::Null),
            (
                json!({"spend":"9223372036854775808","saved":0}),
                Value::Null,
            ),
            (
                json!({"spend":0,"saved":"-9223372036854775809"}),
                Value::Null,
            ),
            (
                json!({"spend":9_223_372_036_854_775_808_u64,"saved":0}),
                Value::Null,
            ),
            (
                serde_json::from_str(r#"{"spend":1.5,"saved":0}"#).unwrap(),
                Value::Null,
            ),
        ] {
            for packed in [pack_metrics(&row), pack_core_metrics(&row)] {
                assert_eq!(packed["original_amount_micro"], original, "{row}");
            }
        }
    }

    #[test]
    fn original_amount_stays_unknown_through_totals_and_other_folding() {
        for (rows, original) in [
            (
                vec![
                    json!({"spend":43200,"saved":-21600}),
                    json!({"spend":-43200,"saved":21600}),
                ],
                json!(0),
            ),
            (
                vec![
                    json!({"spend":100,"saved":20}),
                    json!({"spend":null,"saved":0}),
                ],
                Value::Null,
            ),
            (
                vec![
                    json!({"spend":i64::MAX,"saved":0}),
                    json!({"spend":1,"saved":0}),
                ],
                Value::Null,
            ),
        ] {
            for packed in [
                pack_metrics(&sum_metric_rows(&rows)),
                pack_core_metrics(&sum_core_rows(&rows)),
            ] {
                assert_eq!(packed["original_amount_micro"], original);
                assert!(!packed.contains_key("checked_original_micro"));
            }
            for core in [false, true] {
                let folded = fold_rows(&rows, &|_| FLOW_OTHER.to_owned(), core);
                assert_eq!(folded.len(), 1);
                assert_eq!(folded[0].metrics["original_amount_micro"], original);
            }
            let stacked: Vec<_> = rows
                .into_iter()
                .map(|mut row| {
                    row["bucket"] = json!("2026-10-01");
                    row["k"] = json!("same-model");
                    row
                })
                .collect();
            let (_, data) = fold_stacked(&stacked, 1, "reqs");
            assert_eq!(
                data[0]["values"]["same-model"]["original_amount_micro"],
                original
            );
        }
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
