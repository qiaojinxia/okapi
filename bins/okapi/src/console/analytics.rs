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

use super::stats::{ch_i64, rate_bp};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
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
    /// 回看天数（1–90，缺省 7）。
    #[serde(default)]
    pub days: Option<u32>,
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
    /// 流向图度量（flow 专用）：amount | requests | tokens。
    #[serde(default)]
    pub metric: Option<String>,
    /// 趋势堆叠维度（trend 专用）：model | channel | group | user | api_key。
    #[serde(default)]
    pub stack: Option<String>,
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
        self.days.unwrap_or(7).clamp(1, 90)
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
            clause.push_str(" AND model = {p_model:String}");
            params.push(("p_model".to_owned(), m.to_owned()));
        }
        if let Some(g) = trimmed(self.group.as_deref()) {
            clause.push_str(" AND group_code = {p_group:String}");
            params.push(("p_group".to_owned(), g.to_owned()));
        }
        Scope { clause, params }
    }

    /// 当前窗口 / 等长的上一窗口。环比不是"昨日"那种整日锚点，而是同长度的
    /// 前一段：7 天看板对 7 天，30 天对 30 天，否则周末效应会把对比读歪。
    fn window(&self, previous: bool) -> String {
        let d = self.days();
        if previous {
            format!(
                "hour >= now() - INTERVAL {} DAY AND hour < now() - INTERVAL {d} DAY",
                d * 2
            )
        } else {
            format!("hour >= now() - INTERVAL {d} DAY")
        }
    }
}

/// 立方体全部度量的 Merge 列表；别名刻意与 MV 列名错开。
const AGG: &str = "countMerge(requests) AS reqs, \
                   sumMerge(prompt_tokens) AS prompt, \
                   sumMerge(cached_tokens) AS cached, \
                   sumMerge(completion_tokens) AS completion, \
                   sumMerge(reasoning_tokens) AS reasoning, \
                   sumMerge(amount) AS spend, \
                   sumMerge(discount) AS saved, \
                   sumMerge(upstream_cost) AS cost, \
                   sumMerge(errors) AS errs, \
                   sumMerge(latency_sum) AS lat_sum, \
                   sumMerge(ttft_sum) AS ttft_s, \
                   countIfMerge(ttft_samples) AS ttft_n";

/// 一行聚合 → 展示字段（比率全部基点/整数，避免前端拿浮点二次换算）。
fn pack_metrics(r: &Value) -> serde_json::Map<String, Value> {
    let reqs = ch_i64(r, "reqs");
    let prompt = ch_i64(r, "prompt");
    let cached = ch_i64(r, "cached");
    let completion = ch_i64(r, "completion");
    let errs = ch_i64(r, "errs");
    let ttft_n = ch_i64(r, "ttft_n");
    let mut m = serde_json::Map::new();
    m.insert("requests".into(), json!(reqs));
    m.insert("errors".into(), json!(errs));
    m.insert("error_rate_bp".into(), json!(rate_bp(errs, reqs)));
    m.insert("prompt_tokens".into(), json!(prompt));
    m.insert("cached_tokens".into(), json!(cached));
    m.insert("completion_tokens".into(), json!(completion));
    m.insert("reasoning_tokens".into(), json!(ch_i64(r, "reasoning")));
    m.insert("tokens".into(), json!(prompt.saturating_add(completion)));
    // 口径与门户 breakdown 一致：命中 token / 输入 token
    m.insert("cache_hit_bp".into(), json!(rate_bp(cached, prompt)));
    m.insert("amount_micro".into(), json!(ch_i64(r, "spend")));
    m.insert("discount_micro".into(), json!(ch_i64(r, "saved")));
    m.insert("upstream_cost_micro".into(), json!(ch_i64(r, "cost")));
    m.insert(
        "avg_latency_ms".into(),
        json!(if reqs > 0 {
            ch_i64(r, "lat_sum") / reqs
        } else {
            0
        }),
    );
    m.insert(
        "avg_ttft_ms".into(),
        json!(if ttft_n > 0 {
            ch_i64(r, "ttft_s") / ttft_n
        } else {
            0
        }),
    );
    m
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
    Query(q): Query<CubeQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days();
    let scope = q.scope();
    let params = scope.borrow();

    let (bucket_expr, granularity) = if days <= 2 {
        ("toString(hour)", "hour")
    } else {
        ("toString(toDate(hour))", "day")
    };
    let series_sql = format!(
        "SELECT {bucket_expr} AS bucket, {AGG} FROM mv_cube_hour \
         WHERE {}{} GROUP BY bucket ORDER BY bucket",
        q.window(false),
        scope.clause
    );
    let total_sql = format!(
        "SELECT {AGG} FROM mv_cube_hour WHERE {}{}",
        q.window(false),
        scope.clause
    );
    let prev_sql = format!(
        "SELECT {AGG} FROM mv_cube_hour WHERE {}{}",
        q.window(true),
        scope.clause
    );
    let total = ch.query_with_params(&total_sql, &params).await?;
    let previous = ch.query_with_params(&prev_sql, &params).await?;
    let pack_one = |rows: &[Value]| {
        rows.first()
            .map_or_else(|| json!({}), |r| Value::Object(pack_metrics(r)))
    };

    // 堆叠：按第二维度拆开每个桶（"钱花在哪个模型、占比怎么变"），Top N 之外折进 __other
    if let Some(stack_col) = trimmed(q.stack.as_deref()).map(breakdown_key).transpose()? {
        let limit = q.limit.unwrap_or(8).clamp(1, 20) as usize;
        let stacked_sql = format!(
            "SELECT {bucket_expr} AS bucket, {stack_col} AS k, countMerge(requests) AS reqs, \
                    sumMerge(amount) AS spend, sumMerge(errors) AS errs, \
                    sumMerge(prompt_tokens) + sumMerge(completion_tokens) AS toks \
             FROM mv_cube_hour WHERE {}{} GROUP BY bucket, k ORDER BY bucket",
            q.window(false),
            scope.clause
        );
        let rows = ch.query_with_params(&stacked_sql, &params).await?;
        let (series, data) = fold_stacked(&rows, limit);
        let labels = stack_labels(&state, q.stack.as_deref().unwrap_or_default(), &series).await?;
        return Ok(Json(json!({
            "days": days,
            "granularity": granularity,
            "scope": describe_scope(&state, &q).await?,
            "total": pack_one(&total),
            "previous": pack_one(&previous),
            "stack": q.stack,
            "series": series.iter().map(|k| json!({ "key": k, "label": labels.get(k) })).collect::<Vec<_>>(),
            "data": data,
        })));
    }

    let series = ch.query_with_params(&series_sql, &params).await?;
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
        "scope": describe_scope(&state, &q).await?,
        "total": pack_one(&total),
        "previous": pack_one(&previous),
        "data": data,
    })))
}

/// 堆叠行（bucket × k）→ Top N 序列 + 逐桶数值；其余折进 `__other`。
/// 排名按窗口金额；模型名不进 SQL 省掉 IN 列表转义（与 model_trend 同法）。
fn fold_stacked(rows: &[Value], limit: usize) -> (Vec<String>, Vec<Value>) {
    let mut totals: HashMap<String, i64> = HashMap::new();
    for r in rows {
        *totals.entry(row_key(r)).or_default() += ch_i64(r, "spend");
    }
    let mut ranked: Vec<(String, i64)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top: Vec<String> = ranked.iter().take(limit).map(|(k, _)| k.clone()).collect();
    let has_other = ranked.len() > top.len();

    let mut order: Vec<String> = Vec::new();
    let mut folded: HashMap<String, HashMap<String, [i64; 4]>> = HashMap::new();
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
        cell[0] += ch_i64(r, "reqs");
        cell[1] += ch_i64(r, "spend");
        cell[2] += ch_i64(r, "errs");
        cell[3] += ch_i64(r, "toks");
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
                    cells.get(k).map(|c| {
                        (
                            k.clone(),
                            json!({ "requests": c[0], "amount_micro": c[1], "errors": c[2], "tokens": c[3] }),
                        )
                    })
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
fn breakdown_key(by: &str) -> Result<&'static str, AppError> {
    Ok(match by {
        "model" => "model",
        "channel" | "provider" => "channel_id",
        "user" => "user_id",
        "api_key" => "api_key_id",
        "group" => "group_code",
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

/// 把 CH 行按 `fold_key` 折叠累加。非折叠维度每键恰一行（恒等折叠）；
/// provider 维度多条渠道折成一行——可加列直接相加，比率与均值列折后重算。
fn fold_rows(rows: &[Value], fold_key: &dyn Fn(&str) -> String, refold: bool) -> Vec<Bucket> {
    let mut acc: Vec<Bucket> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for r in rows {
        let key = fold_key(&row_key(r));
        let metrics = pack_metrics(r);
        if let Some(&i) = index.get(&key) {
            let slot = &mut acc[i];
            for (field, v) in &metrics {
                if let (Some(Value::Number(a)), Value::Number(b)) = (slot.metrics.get(field), v)
                    && let (Some(a), Some(b)) = (a.as_i64(), b.as_i64())
                    && !field.ends_with("_bp")
                    && !field.starts_with("avg_")
                {
                    slot.metrics
                        .insert(field.clone(), json!(a.saturating_add(b)));
                }
            }
            slot.folded += 1;
        } else {
            index.insert(key.clone(), acc.len());
            acc.push(Bucket {
                key,
                metrics,
                folded: 1,
            });
        }
    }
    if refold {
        for b in &mut acc {
            let m = &mut b.metrics;
            let reqs = m["requests"].as_i64().unwrap_or(0);
            let errs = m["errors"].as_i64().unwrap_or(0);
            let prompt = m["prompt_tokens"].as_i64().unwrap_or(0);
            let cached = m["cached_tokens"].as_i64().unwrap_or(0);
            m.insert("error_rate_bp".into(), json!(rate_bp(errs, reqs)));
            m.insert("cache_hit_bp".into(), json!(rate_bp(cached, prompt)));
            // 折叠后的平均时延无法从已求平均的行还原，置 0 表示不适用
            m.insert("avg_latency_ms".into(), json!(0));
            m.insert("avg_ttft_ms".into(), json!(0));
        }
        acc.sort_by(|a, b| {
            b.metrics["amount_micro"]
                .as_i64()
                .cmp(&a.metrics["amount_micro"].as_i64())
                .then_with(|| a.key.cmp(&b.key))
        });
    }
    acc
}

/// 上期名次：折叠键 → (金额, 名次)。
fn previous_ranks(
    prev: &[Value],
    fold_key: &dyn Fn(&str) -> String,
) -> HashMap<String, (i64, usize)> {
    let mut spend: BTreeMap<String, i64> = BTreeMap::new();
    for r in prev {
        *spend.entry(fold_key(&row_key(r))).or_default() += ch_i64(r, "spend");
    }
    let mut ranked: Vec<(String, i64)> = spend.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .enumerate()
        .map(|(i, (k, v))| (k, (v, i + 1)))
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

/// 拆分的三条 SQL：当前窗口按键聚合 / 当前窗口汇总（占比分母）/ 上一窗口按键金额。
///
/// provider 要拿全部渠道行来折叠，故不在 SQL 里 LIMIT；其余维度 SQL 侧截断，
/// 占比分母另起一条汇总查询（LIMIT 之外的长尾也要算进分母）。上一窗口只取金额、
/// 不截断：掉出榜单的实体也要给到上期名次。
fn breakdown_sql(
    q: &CubeQuery,
    scope: &Scope,
    key_col: &str,
    fold_provider: bool,
    limit: usize,
) -> [String; 3] {
    let sql_limit = if fold_provider {
        String::new()
    } else {
        format!(" LIMIT {limit}")
    };
    [
        format!(
            "SELECT {key_col} AS k, {AGG} FROM mv_cube_hour WHERE {}{} \
             GROUP BY k ORDER BY spend DESC, k{sql_limit}",
            q.window(false),
            scope.clause
        ),
        format!(
            "SELECT sumMerge(amount) AS spend, countMerge(requests) AS reqs \
             FROM mv_cube_hour WHERE {}{}",
            q.window(false),
            scope.clause
        ),
        format!(
            "SELECT {key_col} AS k, sumMerge(amount) AS spend \
             FROM mv_cube_hour WHERE {}{} GROUP BY k ORDER BY spend DESC, k",
            q.window(true),
            scope.clause
        ),
    ]
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
    Query(q): Query<CubeQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let by = q.by.as_deref().unwrap_or("model");
    let key_col = breakdown_key(by)?;
    let fold_provider = by == "provider";
    let limit = q.limit.unwrap_or(20).clamp(1, 100) as usize;
    let scope = q.scope();
    let params = scope.borrow();

    let [cur_sql, total_sql, prev_sql] = breakdown_sql(&q, &scope, key_col, fold_provider, limit);
    let cur = ch.query_with_params(&cur_sql, &params).await?;
    let total = ch.query_with_params(&total_sql, &params).await?;
    let prev = ch.query_with_params(&prev_sql, &params).await?;

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
    let prev_ranks = previous_ranks(&prev, &fold_key);
    let mut buckets = fold_rows(&cur, &fold_key, fold_provider);
    buckets.truncate(limit);

    let total_spend = total.first().map_or(0, |r| ch_i64(r, "spend"));
    let total_reqs = total.first().map_or(0, |r| ch_i64(r, "reqs"));
    let data: Vec<Value> = buckets
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut m = b.metrics.clone();
            let spend = m["amount_micro"].as_i64().unwrap_or(0);
            let reqs = m["requests"].as_i64().unwrap_or(0);
            let prev = prev_ranks.get(&b.key);
            m.insert("key".into(), json!(b.key));
            m.insert("rank".into(), json!(i + 1));
            m.insert("previous_rank".into(), json!(prev.map(|p| p.1)));
            m.insert(
                "previous_amount_micro".into(),
                json!(prev.map_or(0, |p| p.0)),
            );
            // 环比（基点）：上期为 0 时无意义给 null，前端显示"新"
            m.insert(
                "delta_bp".into(),
                json!(
                    prev.map(|p| p.0)
                        .filter(|p| *p > 0)
                        .map(|p| spend.saturating_sub(p).saturating_mul(10_000) / p)
                ),
            );
            m.insert("share_bp".into(), json!(rate_bp(spend, total_spend)));
            m.insert("request_share_bp".into(), json!(rate_bp(reqs, total_reqs)));
            label_bucket(&mut m, by, b, &names, &owners);
            Value::Object(m)
        })
        .collect();

    Ok(Json(json!({
        "days": q.days(),
        "by": by,
        "scope": describe_scope(&state, &q).await?,
        "total_amount_micro": total_spend,
        "total_requests": total_reqs,
        "data": data,
    })))
}

/// 流向图的五个阶段（new-api Flow 的 user → token → group → model → channel；
/// node 维度我们不做——单机 / 多副本的处理节点对站长没有经营含义）。
const FLOW_STAGES: [&str; 5] = ["user", "api_key", "group", "model", "channel"];
const FLOW_OTHER: &str = "__other";
/// 五维组合取消耗最高的前 N 个；超出即 `truncated`，`coverage_bp` 标注覆盖比例。
const FLOW_ROWS: usize = 5_000;

/// 组合行在某阶段上的节点键。
fn flow_stage_key(r: &Value, stage_name: &str) -> String {
    match stage_name {
        "user" => ch_i64(r, "user_id").to_string(),
        "api_key" => ch_i64(r, "api_key_id").to_string(),
        "group" => ch_str(r, "group_code").to_owned(),
        "model" => ch_str(r, "model").to_owned(),
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
    fn build(combos: &[Value], per_stage: usize, order_col: &str) -> Self {
        let mut stage_totals: Vec<HashMap<String, i64>> = vec![HashMap::new(); FLOW_STAGES.len()];
        let mut covered = 0_i64;
        for r in combos {
            let v = ch_i64(r, order_col);
            covered = covered.saturating_add(v);
            for (i, stage_name) in FLOW_STAGES.iter().enumerate() {
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
            format!("{}:{}", FLOW_STAGES[stage_idx], k)
        };

        let mut nodes: BTreeMap<String, i64> = BTreeMap::new();
        let mut links: BTreeMap<(String, String), i64> = BTreeMap::new();
        for r in combos {
            let v = ch_i64(r, order_col);
            let ids: Vec<String> = FLOW_STAGES
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

/// 节点标签：user / api_key / channel 从 PG 回填，group / model 键即标签，`__other` 为 null。
fn flow_label(stage_name: &str, key: &str, names: &Names) -> Value {
    if key == FLOW_OTHER {
        return Value::Null;
    }
    let id = key.parse::<i64>().ok();
    match stage_name {
        "user" => json!(id.and_then(|k| names.users.get(&k))),
        "api_key" => json!(id.and_then(|k| names.keys.get(&k)).map(|k| k.0.clone())),
        "channel" => json!(id.and_then(|k| names.channels.get(&k)).map(|c| c.0.clone())),
        _ => json!(key),
    }
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
    Query(q): Query<CubeQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
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
    let scope = q.scope();
    let params = scope.borrow();

    let combo_sql = format!(
        "SELECT user_id, api_key_id, group_code, model, channel_id, \
                countMerge(requests) AS reqs, sumMerge(amount) AS spend, \
                sumMerge(prompt_tokens) + sumMerge(completion_tokens) AS toks \
         FROM mv_cube_hour WHERE {}{} \
         GROUP BY user_id, api_key_id, group_code, model, channel_id \
         ORDER BY {order_col} DESC LIMIT {FLOW_ROWS}",
        q.window(false),
        scope.clause
    );
    let total_sql = format!(
        "SELECT countMerge(requests) AS reqs, sumMerge(amount) AS spend, \
                sumMerge(prompt_tokens) + sumMerge(completion_tokens) AS toks \
         FROM mv_cube_hour WHERE {}{}",
        q.window(false),
        scope.clause
    );
    let combos = ch.query_with_params(&combo_sql, &params).await?;
    let total = ch.query_with_params(&total_sql, &params).await?;
    let total_metric = total.first().map_or(0, |r| ch_i64(r, order_col));

    let graph = FlowGraph::build(&combos, per_stage, order_col);
    let names = resolve_names(
        &state,
        &graph.kept_ids(0),
        &graph.kept_ids(1),
        &graph.kept_ids(4),
        &[],
    )
    .await?;

    let nodes: Vec<Value> = graph
        .nodes
        .iter()
        .map(|(id, v)| {
            let (stage_name, key) = id.split_once(':').unwrap_or((id, ""));
            json!({
                "id": id,
                "stage": stage_name,
                "key": key,
                "label": flow_label(stage_name, key, &names),
                "other": key == FLOW_OTHER,
                "value": v,
            })
        })
        .collect();
    let links: Vec<Value> = graph
        .links
        .iter()
        .map(|((s, t), v)| json!({ "source": s, "target": t, "value": v }))
        .collect();

    Ok(Json(json!({
        "days": q.days(),
        "metric": metric,
        "scope": describe_scope(&state, &q).await?,
        "stages": FLOW_STAGES,
        "total": total_metric,
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
    // 今日与窗口分两组聚合，Rust 侧合并——比 sumMergeIf 组合子的可移植性更稳
    let sql = format!(
        "SELECT {col} AS k, day = today() AS is_today, \
                sumMerge(amount) AS spend, countMerge(requests) AS reqs, max(day) AS last_day \
         FROM {table} WHERE {col} IN ({id_list}) AND day >= today() - {days} \
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
    Ok(Json(json!({ "days": days, "data": data })))
}
