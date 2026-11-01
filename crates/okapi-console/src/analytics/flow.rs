//! 流向：用户 → 分组 → 渠道 → 模型的桑基图。
use super::super::query::Query;
use super::super::stats::{ch_i64, rate_bp};
use super::{
    CubeQuery, aggregate_metrics, ch_or_disabled, ch_str, describe_scope, pack_metrics,
    parse_choices,
};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// 流向阶段按调用路径排列；管理员可以隐藏中间阶段，链接从原始组合重新汇总。
pub(super) const FLOW_STAGES: [&str; 6] = ["user", "node", "api_key", "group", "model", "channel"];
pub(super) const FLOW_OTHER: &str = "__other";
/// 五维组合取消耗最高的前 N 个；超出即 `truncated`，`coverage_bp` 标注覆盖比例。
pub(super) const FLOW_ROWS: usize = 5_000;

/// 组合行在某阶段上的节点键。
pub(super) fn flow_stage_key(r: &Value, stage_name: &str) -> String {
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
pub(super) struct FlowGraph {
    pub(super) nodes: BTreeMap<String, i64>,
    pub(super) links: BTreeMap<(String, String), i64>,
    pub(super) keep: Vec<HashSet<String>>,
    pub(super) covered: i64,
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
pub(super) async fn flow_identities(
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
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    q.cached &= super::super::stats_cache::allowed(&headers);
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
    let model_col = q.model_column()?;
    let selected = parse_choices("stages", q.stages.as_deref())?;
    if q.stages.is_some()
        && (selected.len() < 2
            || selected.iter().any(|s| !FLOW_STAGES.contains(&s.as_str()))
            || selected.iter().collect::<HashSet<_>>().len() != selected.len())
    {
        return Err(AppError::bad_request().with_param("stages"));
    }
    q.prepare(&state).await?;
    let agg = aggregate_metrics();
    let current_source = q.source(false);
    let scope = q.scope();
    let params = scope.borrow();
    let stages: Vec<&str> = FLOW_STAGES
        .iter()
        .copied()
        .filter(|s| selected.is_empty() || selected.iter().any(|v| v == s))
        .collect();
    let probe = FLOW_ROWS + 1;
    let combo_sql = format!(
        "SELECT user_id, api_key_id, group_code, {model_col} AS flow_model, channel_id, node, \
                sum(requests) AS reqs, sum(amount) AS spend, \
                sum(prompt_tokens) + sum(completion_tokens) AS toks \
         FROM {current_source} WHERE {}{} \
         GROUP BY user_id, api_key_id, group_code, flow_model, channel_id, node \
         ORDER BY {order_col} DESC LIMIT {probe}",
        q.window(false),
        scope.clause
    );
    let total_sql = format!(
        "SELECT {agg}, sum(prompt_tokens) + sum(completion_tokens) AS toks \
         FROM {current_source} WHERE {}{}",
        q.window(false),
        scope.clause
    );
    let mut combos = ch.query_with_params(&combo_sql, &params).await?;
    // 多取一行判断是否真的截断：恰好 FLOW_ROWS 个组合时不算截断
    let truncated = combos.len() > FLOW_ROWS;
    combos.truncate(FLOW_ROWS);
    let total = ch.query_with_params(&total_sql, &params).await?;
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
        "truncated": truncated,
        "nodes": nodes,
        "links": links,
    })))
}
