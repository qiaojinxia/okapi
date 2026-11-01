//! 拆分：按维度排名的度量表与上一窗口对比。
use super::super::query::Query;
use super::super::stats::{ch_i64, rate_bp};
use super::{
    CubeQuery, Names, Scope, accumulate_core_metrics, accumulate_original, aggregate_metrics,
    describe_scope, pack_core_metrics, pack_metrics, resolve_names,
};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(super) fn breakdown_key(by: &str) -> Result<&'static str, AppError> {
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
pub(super) fn row_key(r: &Value) -> String {
    match r.get("k") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// 拆分结果的一行：折叠键、累加后的度量、折进来的原始行数。
pub(super) struct Bucket {
    pub(super) key: String,
    pub(super) metrics: serde_json::Map<String, Value>,
    pub(super) folded: i64,
}

/// Fold additive raw counters first, then recompute all ratios and coverage once.
pub(super) fn fold_rows(
    rows: &[Value],
    fold_key: &dyn Fn(&str) -> String,
    core: bool,
) -> Vec<Bucket> {
    let mut raw: Vec<(String, Value, i64)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for r in rows {
        let key = fold_key(&row_key(r));
        let pos = *index.entry(key.clone()).or_insert_with(|| {
            raw.push((key, json!({}), 0));
            raw.len() - 1
        });
        let (_, totals, folded) = &mut raw[pos];
        accumulate_original(totals, r);
        super::super::usage_sources::accumulate(totals, r);
        super::super::token_details::accumulate(totals, r);
        super::super::output_rate::accumulate(totals, r);
        // 与趋势堆叠同一份清单：漏一项（曾漏 fixture_reqs / fixture_tokens）就会在排行里恒为 0
        accumulate_core_metrics(totals, r);
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
pub(super) fn previous_ranks(
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
pub(super) fn label_bucket(
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
pub(super) enum BreakdownMetric {
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
pub(super) fn breakdown_sql(
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
        (
            super::super::core_source::AGG.to_owned(),
            q.core_source(false),
        )
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
pub(super) fn ranked_metrics(
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
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let metric = BreakdownMetric::parse(q.metric.as_deref())?;
    q.cached &= super::super::stats_cache::allowed(&headers);
    let by = q.by.as_deref().unwrap_or("model");
    let key_col = if by == "model" {
        q.model_column()?
    } else {
        breakdown_key(by)?
    };
    q.prepare(&state).await?;
    let by = q.by.as_deref().unwrap_or("model");
    let fold_provider = by == "provider";
    let limit = q.limit.unwrap_or(20).clamp(1, 100) as usize;
    let scope = q.scope();
    let params = scope.borrow();

    // 当前排行只要核心指标且精简源能给出同样的值时，不恢复缓存口径（核心指标不含缓存）。
    let core = q.core_requested()? && q.core_source_ok(Some(key_col), false);
    let (cur_sql, total_sql, prev_sql) =
        breakdown_sql(&q, &scope, key_col, fold_provider, limit, metric, core);
    let (cur, total, prev) = tokio::try_join!(
        super::super::stats_cache::query(&state, &cur_sql, &params, q.cached),
        super::super::stats_cache::query(&state, &total_sql, &params, q.cached),
        async {
            match &prev_sql {
                Some(sql) => super::super::stats_cache::query(&state, sql, &params, q.cached).await,
                None => Ok(Vec::new()),
            }
        },
    )?;

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
    let buckets = metric.top_buckets(&cur, &fold_key, core, limit);

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
pub(super) fn ranked_rows(
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
