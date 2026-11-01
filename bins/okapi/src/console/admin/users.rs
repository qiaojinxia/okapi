//! 用户列表、用户总览与用量、消费排行。
use super::super::query::{PageQuery, Query};
use super::{audit, guard};
use crate::gateway::error::AppError;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct LeaderboardQuery {
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// GET /admin/leaderboard：用户消费排行（#1790-11，CH mv_user_day 聚合 + PG 补用户名）。
pub async fn leaderboard(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LeaderboardQuery>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::BILLING_READ).await?;
    let Some(ch) = state.ch.as_ref() else {
        return Err(AppError::new(
            StatusCode::NOT_IMPLEMENTED,
            codes::STATS_DISABLED,
        ));
    };
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let sql = format!(
        "SELECT user_id, countMerge(requests) AS requests, \
                sumMerge(tokens) AS tokens, sumMerge(amount) AS amount_micro \
         FROM mv_user_day WHERE day >= today() - {days} \
         GROUP BY user_id ORDER BY amount_micro DESC LIMIT {limit}"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;

    // 用户名补齐（PG 点查；榜单 ≤100 行）
    let ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| {
            r.get("user_id")
                .and_then(|v| v.as_str().map_or_else(|| v.as_i64(), |s| s.parse().ok()))
        })
        .collect();
    let names = sqlx::query!(r#"SELECT id, username FROM users WHERE id = ANY($1)"#, &ids)
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let name_of: std::collections::HashMap<i64, String> =
        names.into_iter().map(|r| (r.id, r.username)).collect();

    let data: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            let uid = r
                .get("user_id")
                .and_then(|v| v.as_str().map_or_else(|| v.as_i64(), |s| s.parse().ok()))
                .unwrap_or(0);
            if let Some(obj) = r.as_object_mut() {
                obj.insert(
                    "username".into(),
                    json!(name_of.get(&uid).cloned().unwrap_or_default()),
                );
            }
            r
        })
        .collect();
    Ok(Json(json!({ "days": days, "data": data })))
}

// ---- 代客查看（#1790-2，强审计）----

/// GET /admin/users：用户列表（此前只能按 ID 操作，没有列表入口）。`?q=` 匹配用户名 / 邮箱，
/// `limit/offset` 与令牌 / 兑换码同一套 `PageQuery`（大表：不传 limit 缺省 `DEFAULT_LIMIT`）。
/// 只读点 `user.read` 即可（与 /admin/keys 列表一致，§11.6 读写分离）：此前守的是
/// `user.manage`，只读运营角色在侧栏看得见"用户"却点进去 403——文档、导航与后端三处
/// 里后端是那个错的。管理动作（manage/credit/role/groups）仍要 `user.manage`。
pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::USER_READ).await?;
    let slice = q.bounded();
    let (limit, offset) = (slice.capped_limit(), slice.offset);
    // 空查询用 NULL 表示"不过滤"，避免 '%%' 走不上索引的语义歧义；`%` `_` 已转义
    let needle = okapi_store::listing::like_pattern(q.keyword());

    // 大表：limit 被封顶，COUNT 不能省，与数据查询并行
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT id, username, email, role, status, balance_micro, admin_role_id,
               price_multiplier::text AS "multiplier!", created_at
        FROM users
        WHERE deleted_at IS NULL
          AND ($1::text IS NULL OR username ILIKE $1 OR email ILIKE $1)
        ORDER BY id DESC
        LIMIT $2 OFFSET $3
        "#,
            needle,
            limit,
            offset
        )
        .fetch_all(&state.pg),
        sqlx::query_scalar!(
            r#"
        SELECT COUNT(*)::bigint AS "c!" FROM users
        WHERE deleted_at IS NULL
          AND ($1::text IS NULL OR username ILIKE $1 OR email ILIKE $1)
        "#,
            needle
        )
        .fetch_one(&state.pg),
    )
    .map_err(okapi_store::StoreError::from)?;

    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "username": r.username,
                "email": r.email,
                "role": r.role,
                "status": r.status,
                "balance_micro": r.balance_micro,
                "admin_role_id": r.admin_role_id,
                "price_multiplier": r.multiplier,
                "created_at": r.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "total": total, "data": data })))
}

pub async fn user_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_ASSIST).await?;
    let Some(user) = sqlx::query!(
        r#"SELECT id, username, role, status, balance_micro, price_multiplier::text AS "multiplier!"
           FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    let groups = sqlx::query!(
        r#"SELECT group_code, priority FROM user_groups WHERE user_id = $1 ORDER BY priority DESC"#,
        user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let keys = sqlx::query!(
        r#"SELECT id, name, key_prefix, status, used_micro FROM api_keys
           WHERE user_id = $1 AND deleted_at IS NULL ORDER BY id"#,
        user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let hot_balance = state.ledger.balance(user_id).await?;

    // 代客操作必须留痕（谁看了谁）
    audit(
        &state,
        &actor,
        "user.assist.view",
        &user_id.to_string(),
        json!({}),
    )
    .await;

    Ok(Json(json!({
        "user": {
            "id": user.id, "username": user.username, "role": user.role,
            "status": user.status, "balance_micro": user.balance_micro,
            "hot_balance_micro": hot_balance.as_micros(),
            "price_multiplier": user.multiplier,
        },
        "groups": groups.iter().map(|g| json!({"code": g.group_code, "priority": g.priority})).collect::<Vec<_>>(),
        "keys": keys.iter().map(|k| json!({
            "id": k.id, "name": k.name, "key_prefix": k.key_prefix,
            "status": k.status, "used_micro": k.used_micro,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct UserUsageQuery {
    #[serde(default)]
    pub days: Option<u32>,
}

/// GET /admin/users/{id}/usage：代客视角的用量与余额变动史（老 ok-api 用户详情
/// UsageOverviewTab / UsageTrendChart 的吸收）。
///
/// 管理员调余额、处理"为什么扣这么多"之前要先看两件事：他平时花多少（按日/按模型，
/// CH mv_key_model_day 用户前缀）与上次动过什么账（PG billing_events，含 actor——
/// 这是管理面，谁调的账要看得见，与门户端点隐去管理员 id 相反）。
/// CH 未启用时用量部分为空数组而非整体 501：余额变动史不依赖 CH，不该被连坐。
pub async fn user_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Query(q): Query<UserUsageQuery>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_ASSIST).await?;
    let days = q.days.unwrap_or(7).clamp(1, 90);

    let mut daily: Vec<Value> = Vec::new();
    let mut by_model: Vec<Value> = Vec::new();
    if let Some(ch) = state.ch.as_ref() {
        let rows = ch
            .query_json_each_row(&format!(
                "SELECT day, model, countMerge(requests) AS reqs, sumMerge(amount) AS spend, \
                        sumMerge(prompt_tokens) + sumMerge(completion_tokens) AS toks \
                 FROM mv_key_model_day WHERE user_id = {user_id} AND day >= today() - {days} \
                 GROUP BY day, model ORDER BY day"
            ))
            .await?;
        // BTreeMap：按日键有序输出，前端直接画不用再排
        let mut per_day: std::collections::BTreeMap<String, (i64, i64)> =
            std::collections::BTreeMap::new();
        let mut per_model: std::collections::HashMap<String, (i64, i64, i64)> =
            std::collections::HashMap::new();
        for r in &rows {
            let day = r.get("day").and_then(Value::as_str).unwrap_or_default();
            let model = r.get("model").and_then(Value::as_str).unwrap_or_default();
            let (reqs, spend, toks) = (
                super::super::stats::ch_i64(r, "reqs"),
                super::super::stats::ch_i64(r, "spend"),
                super::super::stats::ch_i64(r, "toks"),
            );
            let d = per_day.entry(day.to_owned()).or_default();
            d.0 += reqs;
            d.1 += spend;
            let m = per_model.entry(model.to_owned()).or_default();
            m.0 += reqs;
            m.1 += spend;
            m.2 += toks;
        }
        daily = per_day
            .into_iter()
            .map(|(day, (reqs, spend))| json!({ "day": day, "requests": reqs, "amount_micro": spend }))
            .collect();
        let mut models: Vec<(String, (i64, i64, i64))> = per_model.into_iter().collect();
        models.sort_by_key(|m| std::cmp::Reverse(m.1.1));
        by_model = models
            .into_iter()
            .take(10)
            .map(|(model, (reqs, spend, toks))| {
                json!({ "model": model, "requests": reqs, "amount_micro": spend, "tokens": toks })
            })
            .collect();
    }

    let events = sqlx::query!(
        r#"SELECT event_id, event_type, delta_micro, balance_after_micro, payload, actor, created_at
           FROM billing_events
           WHERE user_id = $1
             AND event_type IN ('recharge', 'adjust', 'refund', 'expire')
             AND delta_micro <> 0
           ORDER BY event_id DESC LIMIT 10"#,
        user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let ledger: Vec<Value> = events
        .into_iter()
        .map(|e| {
            json!({
                "event_id": e.event_id,
                "event_type": e.event_type,
                "delta_micro": e.delta_micro,
                "balance_after_micro": e.balance_after_micro,
                "actor": e.actor,
                "tags": e.payload.as_ref().and_then(|p| p.get("tags")).cloned().unwrap_or(Value::Array(vec![])),
                "reason": e.payload.as_ref().and_then(|p| p.get("reason")).cloned(),
                "created_at": e.created_at.to_rfc3339(),
            })
        })
        .collect();

    audit(
        &state,
        &actor,
        "user.assist.usage",
        &user_id.to_string(),
        json!({ "days": days }),
    )
    .await;

    Ok(Json(json!({
        "days": days,
        "stats_available": state.ch.is_some(),
        "daily": daily,
        "by_model": by_model,
        "ledger": ledger,
    })))
}
