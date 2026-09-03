//! 审计日志读取面（IMPLEMENTATION §11.15）。
//!
//! 每个管理写操作与 MCP 写工具都会落 `audit_logs`（actor / action / target / detail），
//! 登录成功与失败也在此记录（`user.login` / `user.login_failed`）。此前只有写入、没有任何
//! 读取出口——多管理员 + 自定义角色的站点里"谁改了价、谁封了这个用户、谁在试我的密码"
//! 无处可查，写审计等于没写。
//!
//! 过滤参数全部走 sqlx 绑定参数（PG 侧转义），`action` 按前缀匹配（`user.` 拿到全部
//! 用户类动作），翻页用 `before`（id 倒序游标）而非 offset——审计表按月分区且只增，
//! 游标翻页在翻页期间新写入的行不会造成重叠。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use okapi_api::permissions;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    /// 操作者原文（`admin:42` / `user:7` / `mcp:3` / `anon`）。
    #[serde(default)]
    pub actor: Option<String>,
    /// 动作前缀（`channel.` / `user.login`）。
    #[serde(default)]
    pub action: Option<String>,
    /// 对象精确匹配（渠道 id / 用户 id / 分组码 / 邮箱）。
    #[serde(default)]
    pub target: Option<String>,
    /// 相对窗口小时数（缺省 168 = 7 天，上限 90 天）；给了 from 则忽略。
    #[serde(default)]
    pub hours: Option<u32>,
    #[serde(default)]
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    /// 游标：取该 id 之前的记录。
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

fn trimmed(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 操作者标签回填：`admin:42` → 用户名；`mcp:3` → key 名 + 属主；`user:7` → 用户名。
/// 原文保留在 `actor` 字段——审计要能精确复现，标签只是给人看的。
async fn resolve_actors(
    state: &AppState,
    actors: &[String],
) -> Result<HashMap<String, Value>, AppError> {
    let mut user_ids: Vec<i64> = Vec::new();
    let mut key_ids: Vec<i64> = Vec::new();
    for a in actors {
        if let Some((kind, id)) = a.split_once(':')
            && let Ok(id) = id.parse::<i64>()
        {
            match kind {
                "admin" | "user" => user_ids.push(id),
                "mcp" => key_ids.push(id),
                _ => {}
            }
        }
    }
    user_ids.sort_unstable();
    user_ids.dedup();
    key_ids.sort_unstable();
    key_ids.dedup();

    let users: HashMap<i64, String> = if user_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query!(
            r#"SELECT id, username FROM users WHERE id = ANY($1)"#,
            &user_ids
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .into_iter()
        .map(|r| (r.id, r.username))
        .collect()
    };
    let keys: HashMap<i64, (String, String)> = if key_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query!(
            r#"SELECT k.id, k.name, u.username FROM api_keys k JOIN users u ON u.id = k.user_id
               WHERE k.id = ANY($1)"#,
            &key_ids
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .into_iter()
        .map(|r| (r.id, (r.name, r.username)))
        .collect()
    };

    Ok(actors
        .iter()
        .map(|a| {
            let (kind, id) = a.split_once(':').unwrap_or((a.as_str(), ""));
            let id_num = id.parse::<i64>().ok();
            let label = match kind {
                "admin" | "user" => id_num.and_then(|i| users.get(&i)).cloned(),
                "mcp" => id_num
                    .and_then(|i| keys.get(&i))
                    .map(|(name, owner)| format!("{owner} / {name}")),
                _ => None,
            };
            (
                a.clone(),
                json!({ "kind": kind, "id": id_num, "label": label }),
            )
        })
        .collect())
}

/// GET /admin/audit：审计检索（倒序、游标翻页、四维过滤）。
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::AUDIT_READ).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let hours = i64::from(q.hours.unwrap_or(168).clamp(1, 24 * 90));
    let from = q
        .from
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::hours(hours));
    let to = q.to;
    let actor = trimmed(q.actor.as_deref());
    let action = trimmed(q.action.as_deref());
    let target = trimmed(q.target.as_deref());

    let rows = sqlx::query!(
        r#"
        SELECT id, actor, action, target, detail, host(ip) AS ip, created_at
        FROM audit_logs
        WHERE created_at >= $1
          AND ($2::timestamptz IS NULL OR created_at < $2)
          AND ($3::text IS NULL OR actor = $3)
          AND ($4::text IS NULL OR action LIKE $4 || '%')
          AND ($5::text IS NULL OR target = $5)
          AND ($6::bigint IS NULL OR id < $6)
        ORDER BY id DESC
        LIMIT $7
        "#,
        from,
        to,
        actor,
        action,
        target,
        q.before,
        limit
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let actors: Vec<String> = rows.iter().map(|r| r.actor.clone()).collect();
    let labels = resolve_actors(&state, &actors).await?;
    let has_more = i64::try_from(rows.len()).unwrap_or(0) == limit;
    let next_before = rows.last().map(|r| r.id);
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "actor": r.actor,
                "actor_info": labels.get(&r.actor),
                "action": r.action,
                "target": r.target,
                "detail": r.detail,
                "ip": r.ip,
                "created_at": r.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "data": data,
        "has_more": has_more,
        "next_before": if has_more { next_before } else { None },
    })))
}

/// GET /admin/audit/actions：近 90 天出现过的动作名（过滤下拉的数据源）。
/// 动作名散落在各处理函数里，没有一份静态清单；从数据反推比维护常量表更不会漂移。
pub async fn actions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::AUDIT_READ).await?;
    let rows = sqlx::query_scalar!(
        r#"SELECT DISTINCT action FROM audit_logs
           WHERE created_at >= now() - interval '90 days' ORDER BY action"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({ "data": rows })))
}

/// 登录审计（成功 / 失败）。失败时按邮箱反查用户：真实用户要能在门户看到
/// "有人在试我的密码"，所以只要邮箱存在就记到该用户名下；邮箱不存在记 `anon`。
/// 写失败只打日志——审计不能反过来把登录拖垮。
pub async fn record_login(
    state: &AppState,
    email: &str,
    user_id: Option<i64>,
    ok: bool,
    reason: Option<&str>,
    ip: Option<String>,
    headers: &HeaderMap,
) {
    let resolved = if user_id.is_some() {
        user_id
    } else {
        sqlx::query_scalar!(
            r#"SELECT id FROM users WHERE email = $1 AND deleted_at IS NULL"#,
            email
        )
        .fetch_optional(&state.pg)
        .await
        .ok()
        .flatten()
    };
    let actor = resolved.map_or_else(|| "anon".to_owned(), |id| format!("user:{id}"));
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(200).collect::<String>());
    let action = if ok {
        "user.login"
    } else {
        "user.login_failed"
    };
    if let Err(err) = okapi_store::admin::record_audit(
        &state.pg,
        &actor,
        action,
        email,
        json!({ "ip": ip, "ua": ua, "reason": reason }),
    )
    .await
    {
        tracing::error!(error = %err, action, "登录审计写入失败");
    }
}

/// GET /api/me/logins：我最近的登录记录（成功与失败各带 IP / 时间）。
/// new-api 的"登录会话"卡的对应物之一：不做会话枚举，先把"最近谁登过、谁试过密码"
/// 给到用户——这是共享设备与撞库场景下最先要回答的问题。
pub async fn my_logins(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    let actor = format!("user:{}", key.user_id);
    let rows = sqlx::query!(
        r#"SELECT action, detail, created_at FROM audit_logs
           WHERE actor = $1 AND action IN ('user.login', 'user.login_failed')
           ORDER BY id DESC LIMIT 20"#,
        actor
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let detail = r.detail.unwrap_or(Value::Null);
            json!({
                "ok": r.action == "user.login",
                "at": r.created_at,
                "ip": detail.get("ip").cloned().unwrap_or(Value::Null),
                "ua": detail.get("ua").cloned().unwrap_or(Value::Null),
                "reason": detail.get("reason").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}
