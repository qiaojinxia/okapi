//! Team 层管理面（IMPLEMENTATION §6.1 定案）：team 即 user 主体（kind='team'），
//! 成员经 web session 鉴权自助操作；团 key 归属成员（member_user_id）。
//! 钱包入账走既有 admin credit（team 的 user_id 即钱包主体）。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Cookie 会话 → user_id（复用 auth_web 的会话存储）。
async fn require_session(state: &AppState, headers: &HeaderMap) -> Result<i64, AppError> {
    let sid = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|pair| {
                let (name, value) = pair.trim().split_once('=')?;
                (name == "okapi_session").then(|| value.to_owned())
            })
        })
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))?;
    state
        .sched
        .web_session_get(&sid)
        .await
        .ok_or_else(|| AppError::unauthorized(okapi_api::codes::INVALID_API_KEY))
}

/// 成员角色查询；None = 非成员。
async fn member_role(state: &AppState, team: i64, member: i64) -> Result<Option<String>, AppError> {
    let role = sqlx::query_scalar!(
        r#"SELECT role FROM team_members WHERE team_user_id = $1 AND member_user_id = $2"#,
        team,
        member
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(role)
}

fn can_manage(role: Option<&str>) -> bool {
    matches!(role, Some("owner" | "admin"))
}

// ---- 建团 ----

#[derive(Deserialize)]
pub struct CreateTeamReq {
    pub name: String,
}

/// POST /api/teams：建 team 用户（kind=team）+ 创建人入 owner。
pub async fn create_team(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateTeamReq>,
) -> Result<Json<Value>, AppError> {
    let creator = require_session(&state, &headers).await?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 48 {
        return Err(AppError::bad_request().with_param("name"));
    }
    let unique = format!(
        "team:{name}:{}",
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    );
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let team_id = sqlx::query_scalar!(
        r#"INSERT INTO users (username, kind) VALUES ($1, 'team') RETURNING id"#,
        unique
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(okapi_store::StoreError::from)?;
    sqlx::query!(
        r#"INSERT INTO team_members (team_user_id, member_user_id, role) VALUES ($1, $2, 'owner')"#,
        team_id,
        creator
    )
    .execute(&mut *tx)
    .await
    .map_err(okapi_store::StoreError::from)?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({ "team_id": team_id, "name": name })))
}

/// 建团时 username 存为 `team:{name}:{短uuid}`（保证全局唯一）；此处反解出展示名。
/// 取中间段而非首尾，因为团名本身可能含 `:`。
fn display_name(username: &str) -> String {
    let body = username.strip_prefix("team:").unwrap_or(username);
    body.rsplit_once(':')
        .map_or(body, |(name, _)| name)
        .to_owned()
}

/// GET /api/teams：我所属的团队列表（UI 入口——没有它前端无从知道自己在哪些团）。
pub async fn list_my_teams(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let me = require_session(&state, &headers).await?;
    let rows = sqlx::query!(
        r#"
        SELECT tm.team_user_id, u.username, tm.role, tm.monthly_spend_limit_micro,
               (SELECT COUNT(*) FROM team_members x WHERE x.team_user_id = tm.team_user_id)
                   AS "member_count!"
        FROM team_members tm
        JOIN users u ON u.id = tm.team_user_id
        WHERE tm.member_user_id = $1 AND u.deleted_at IS NULL
        ORDER BY tm.team_user_id
        "#,
        me
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut data = Vec::with_capacity(rows.len());
    for r in rows {
        // 团钱包余额走热账本（团数量少，逐个查可接受）
        let balance = state.ledger.balance(r.team_user_id).await?;
        data.push(json!({
            "team_id": r.team_user_id,
            "name": display_name(&r.username),
            "role": r.role,
            "member_count": r.member_count,
            "monthly_spend_limit_micro": r.monthly_spend_limit_micro,
            "balance_micro": balance.as_micros(),
        }));
    }
    Ok(Json(json!({ "data": data })))
}

// ---- 成员管理 ----

#[derive(Deserialize)]
pub struct AddMemberReq {
    pub user_id: i64,
    #[serde(default = "default_member_role")]
    pub role: String,
    #[serde(default)]
    pub monthly_spend_limit_micro: Option<i64>,
}

fn default_member_role() -> String {
    "member".to_owned()
}

/// POST /api/teams/{id}/members：owner/admin 加人或调限额（upsert）。
pub async fn upsert_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(team_id): Path<i64>,
    Json(req): Json<AddMemberReq>,
) -> Result<Json<Value>, AppError> {
    let actor = require_session(&state, &headers).await?;
    let role = member_role(&state, team_id, actor).await?;
    if !can_manage(role.as_deref()) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            okapi_api::codes::PERMISSION_DENIED,
        ));
    }
    if !matches!(req.role.as_str(), "admin" | "member") {
        return Err(AppError::bad_request().with_param("role"));
    }
    sqlx::query!(
        r#"
        INSERT INTO team_members (team_user_id, member_user_id, role, monthly_spend_limit_micro)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (team_user_id, member_user_id) DO UPDATE SET
            role = EXCLUDED.role,
            monthly_spend_limit_micro = EXCLUDED.monthly_spend_limit_micro
        "#,
        team_id,
        req.user_id,
        req.role,
        req.monthly_spend_limit_micro
    )
    .execute(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    // 团 key 的鉴权缓存（含限额快照）失效
    state.sched.auth_flush().await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 成员自助发团 key ----

#[derive(Deserialize)]
pub struct TeamKeyReq {
    #[serde(default)]
    pub name: Option<String>,
}

/// POST /api/teams/{id}/keys：任意成员给自己发团 key（扣团钱包，明文一次）。
pub async fn create_team_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(team_id): Path<i64>,
    Json(req): Json<TeamKeyReq>,
) -> Result<Json<Value>, AppError> {
    let member = require_session(&state, &headers).await?;
    if member_role(&state, team_id, member).await?.is_none() {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            okapi_api::codes::PERMISSION_DENIED,
        ));
    }
    let token = format!(
        "sk-okapi-{}",
        rand::rng()
            .sample_iter(&Alphanumeric)
            .take(43)
            .map(char::from)
            .collect::<String>()
    );
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let name = req.name.as_deref().unwrap_or("team");
    let key_id = sqlx::query_scalar!(
        r#"INSERT INTO api_keys (user_id, key_hash, key_prefix, name, member_user_id)
           VALUES ($1, $2, $3, $4, $5) RETURNING id"#,
        team_id,
        key_hash,
        token.chars().take(16).collect::<String>(),
        name,
        member
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({
        "key_id": key_id,
        // 明文仅本次返回
        "api_key": token,
    })))
}

// ---- 成员分账 ----

/// GET /api/teams/{id}/usage：按成员分账（PG used_micro 累计 + 本月 Redis 计数）。
pub async fn team_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(team_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = require_session(&state, &headers).await?;
    if member_role(&state, team_id, actor).await?.is_none() {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            okapi_api::codes::PERMISSION_DENIED,
        ));
    }
    let rows = sqlx::query!(
        r#"
        SELECT tm.member_user_id, u.username, tm.role, tm.monthly_spend_limit_micro,
               COALESCE(SUM(k.used_micro), 0)::bigint AS "used_micro!"
        FROM team_members tm
        JOIN users u ON u.id = tm.member_user_id
        LEFT JOIN api_keys k
               ON k.user_id = tm.team_user_id AND k.member_user_id = tm.member_user_id
              AND k.deleted_at IS NULL
        WHERE tm.team_user_id = $1
        GROUP BY tm.member_user_id, u.username, tm.role, tm.monthly_spend_limit_micro
        ORDER BY tm.member_user_id
        "#,
        team_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut data = Vec::with_capacity(rows.len());
    for r in rows {
        let month_spend = state
            .sched
            .member_spend_get(team_id, r.member_user_id)
            .await;
        data.push(json!({
            "member_user_id": r.member_user_id,
            "username": r.username,
            "role": r.role,
            "monthly_spend_limit_micro": r.monthly_spend_limit_micro,
            "total_spend_micro": r.used_micro,
            "month_spend_micro": month_spend,
        }));
    }
    let balance = state.ledger.balance(team_id).await?;
    Ok(Json(json!({
        "team_id": team_id,
        "balance_micro": balance.as_micros(),
        "members": data,
    })))
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn display_name_strips_uniqueness_suffix() {
        assert_eq!(display_name("team:Acme:a1b2c3"), "Acme");
        // 团名含冒号时只剥最后一段后缀
        assert_eq!(display_name("team:a:b:ffffff"), "a:b");
        // 非团格式（防御）原样返回
        assert_eq!(display_name("plain"), "plain");
    }
}
