//! 资产清点与单个实体（用户 / 渠道 / key）的用量。
use super::super::query::Query;
use super::super::stats::ch_i64;
use super::{ch_or_disabled, ch_str};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

/// 渠道 key 六态各多少把（§3.4 状态机：1 可用 / 2 冷却 / 3 限速 / 4 额度耗尽 / 5 封禁 / 6 凭证无效）。
pub(super) async fn channel_key_status_counts(pg: &sqlx::PgPool) -> Result<Value, AppError> {
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
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
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
    super::super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
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

pub(super) async fn enrich_entities(
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
    super::super::usage_sources::enrich(ch, column, table, predicate, &mut rows).await?;
    super::super::output_rate::enrich_entities(ch, &mut rows, column, table, predicate).await?;
    for mut row in rows {
        let id = row[column].as_str().unwrap_or_default().to_owned();
        if let Some(fields) = row.as_object_mut() {
            fields.remove(column);
        }
        data.insert(id, row);
    }
    Ok(())
}

pub(super) fn parse_choices(name: &str, value: Option<&str>) -> Result<Vec<String>, AppError> {
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
