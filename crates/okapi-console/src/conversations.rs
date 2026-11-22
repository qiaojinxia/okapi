//! 对话内容存档的查询、统计、清理与删除端点（同时兼容 ClickHouse 与纯 PostgreSQL 模式）。

use crate::admin::{audit, guard};
use crate::console_auth::authenticate;
use crate::query::Query;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Path;
use okapi_store::conversation_archive::{
    ArchiveFilter, ArchiveSummary, delete_by_request_id, get_by_request_id, list, prune, stats,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Deserialize, Default)]
pub struct AdminArchiveQuery {
    pub user_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub search: Option<String>,
    pub hours: Option<u32>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Deserialize, Default)]
pub struct PortalArchiveQuery {
    /// `key` = 仅当前令牌；`user` = 当前用户全部令牌。
    pub scope: Option<String>,
    pub api_key_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub search: Option<String>,
    pub hours: Option<u32>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

fn parse_optional_uuid(raw: Option<&str>) -> Result<Option<Uuid>, AppError> {
    let Some(trimmed) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    Uuid::parse_str(trimmed)
        .map(Some)
        .map_err(|_| AppError::bad_request().with_param("request_id"))
}

fn parse_path_uuid(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw.trim()).map_err(|_| AppError::bad_request().with_param("request_id"))
}

async fn enrich_admin_summaries(
    state: &AppState,
    rows: &[ArchiveSummary],
) -> Result<Vec<Value>, AppError> {
    let mut user_ids: Vec<i64> = rows.iter().map(|r| r.user_id).filter(|&id| id > 0).collect();
    user_ids.sort_unstable();
    user_ids.dedup();
    let mut channel_ids: Vec<i64> = rows
        .iter()
        .map(|r| r.channel_id)
        .filter(|&id| id > 0)
        .collect();
    channel_ids.sort_unstable();
    channel_ids.dedup();

    let mut users: HashMap<i64, String> = HashMap::new();
    if !user_ids.is_empty() {
        let fetched: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, username FROM users WHERE id = ANY($1)")
                .bind(&user_ids)
                .fetch_all(&state.pg)
                .await
                .map_err(okapi_store::StoreError::from)?;
        for (id, name) in fetched {
            users.insert(id, name);
        }
    }

    let mut channels: HashMap<i64, String> = HashMap::new();
    if !channel_ids.is_empty() {
        let fetched: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, name FROM channels WHERE id = ANY($1)")
                .bind(&channel_ids)
                .fetch_all(&state.pg)
                .await
                .map_err(okapi_store::StoreError::from)?;
        for (id, name) in fetched {
            channels.insert(id, name);
        }
    }

    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "request_id": r.request_id,
                "user_id": r.user_id,
                "username": users.get(&r.user_id).cloned().unwrap_or_default(),
                "api_key_id": r.api_key_id,
                "channel_id": r.channel_id,
                "channel_name": channels.get(&r.channel_id).cloned().unwrap_or_default(),
                "model": r.model,
                "requested_model": r.requested_model,
                "endpoint": r.endpoint,
                "is_stream": r.is_stream,
                "status_code": r.status_code,
                "error_code": r.error_code,
                "request_preview": r.request_preview,
                "response_preview": r.response_preview,
                "truncated": r.truncated,
                "created_at": r.created_at,
                "storage": r.storage,
            })
        })
        .collect())
}

/// GET /admin/conversations：管理员检索全站对话存档列表（ClickHouse 与纯 PG 模式均可用）。
pub async fn admin_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AdminArchiveQuery>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::BILLING_READ).await?;
    let request_id = parse_optional_uuid(q.request_id.as_deref())?;
    let filter = ArchiveFilter {
        user_id: q.user_id,
        api_key_id: q.api_key_id,
        model: q.model,
        request_id,
        search: q.search,
        hours: q.hours,
        limit: q.limit.unwrap_or(50).clamp(1, 200),
        offset: q.offset.unwrap_or(0).min(10_000),
    };
    let rows = list(&state.pg, state.ch.as_ref(), &filter).await?;
    let data = enrich_admin_summaries(&state, &rows).await?;
    let backend = if state.ch.is_some() {
        "clickhouse"
    } else {
        "postgres"
    };
    Ok(Json(json!({
        "data": data,
        "backend": backend,
        "clickhouse_enabled": state.ch.is_some(),
    })))
}

/// GET /admin/conversations/stats：当前存档后端、条数、时间跨度及生效策略。
pub async fn admin_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::BILLING_READ).await?;
    let policy = state.conversation_archive_policy().await;
    let s = stats(&state.pg, state.ch.as_ref()).await?;
    Ok(Json(json!({
        "backend": s.backend,
        "clickhouse_enabled": s.clickhouse_enabled,
        "total_count": s.total_count,
        "pg_count": s.pg_count,
        "ch_count": s.ch_count,
        "oldest_at": s.oldest_at,
        "newest_at": s.newest_at,
        "policy": policy,
    })))
}

/// POST /admin/conversations/cleanup：按当前 `retention_days` 立即清理过期对话存档。
pub async fn admin_cleanup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    let stored = state
        .setting_fresh(okapi_kernel::policies::CONVERSATION_ARCHIVE_SETTING)
        .await?;
    let policy =
        okapi_kernel::policies::ConversationArchivePolicy::from_setting(stored.as_ref());
    if policy.retention_days == 0 {
        return Err(AppError::bad_request().with_param("retention_permanent"));
    }
    let report = prune(
        &state.pg,
        state.ch.as_ref(),
        policy.retention_days,
        chrono::Utc::now(),
    )
    .await?;
    audit(
        &state,
        &actor,
        "conversation_archive.cleanup",
        "conversation_archives",
        json!({
            "retention_days": policy.retention_days,
            "pg_deleted": report.pg_deleted,
            "ch_deleted_requested": report.ch_deleted_requested,
            "cutoff": report.cutoff,
        }),
    )
    .await;
    Ok(Json(json!({
        "ok": true,
        "retention_days": policy.retention_days,
        "pg_deleted": report.pg_deleted,
        "ch_deleted_requested": report.ch_deleted_requested,
        "cutoff": report.cutoff,
    })))
}

/// GET /admin/logs/{request_id}/conversation：按请求 ID 读取完整对话存档。
pub async fn admin_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::BILLING_READ).await?;
    let request_id = parse_path_uuid(&raw_id)?;
    let Some(record) = get_by_request_id(&state.pg, state.ch.as_ref(), request_id, None).await?
    else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    Ok(Json(json!({ "data": record })))
}

/// DELETE /admin/conversations/{request_id}：删除指定单条对话存档（同时清理 PG 与 CH）。
pub async fn admin_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    let request_id = parse_path_uuid(&raw_id)?;
    let deleted = delete_by_request_id(&state.pg, state.ch.as_ref(), request_id).await?;
    audit(
        &state,
        &actor,
        "conversation_archive.delete",
        &request_id.to_string(),
        json!({ "deleted": deleted }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "deleted": deleted })))
}

/// GET /api/me/conversations/policy：门户查询是否允许查看本人对话存档。
pub async fn portal_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let _ = authenticate(&state, &headers).await?;
    let policy = state.conversation_archive_policy().await;
    Ok(Json(json!({
        "enabled": policy.enabled,
        "allow_user_view": policy.allow_user_view,
    })))
}

/// GET /api/me/conversations：门户用户分页查询自己的对话存档（需 `allow_user_view = true`）。
pub async fn portal_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PortalArchiveQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let policy = state.conversation_archive_policy().await;
    if !policy.allow_user_view {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("conversation_archive"),
        );
    }
    let request_id = parse_optional_uuid(q.request_id.as_deref())?;
    let api_key_id = match q.scope.as_deref() {
        Some("user") => q.api_key_id,
        _ => Some(key.key_id),
    };
    let filter = ArchiveFilter {
        user_id: Some(key.user_id),
        api_key_id,
        model: q.model,
        request_id,
        search: q.search,
        hours: q.hours,
        limit: q.limit.unwrap_or(50).clamp(1, 200),
        offset: q.offset.unwrap_or(0).min(10_000),
    };
    let rows = list(&state.pg, state.ch.as_ref(), &filter).await?;
    Ok(Json(json!({
        "data": rows,
        "backend": if state.ch.is_some() { "clickhouse" } else { "postgres" },
    })))
}

/// GET /api/me/logs/{request_id}/conversation：门户用户读取自己某笔请求的对话存档。
pub async fn portal_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let policy = state.conversation_archive_policy().await;
    if !policy.allow_user_view {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("conversation_archive"),
        );
    }
    let request_id = parse_path_uuid(&raw_id)?;
    let Some(record) =
        get_by_request_id(&state.pg, state.ch.as_ref(), request_id, Some(key.user_id)).await?
    else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    Ok(Json(json!({ "data": record })))
}
