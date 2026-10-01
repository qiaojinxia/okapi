//! 死信队列（billing_dlq）的控制面出口。
//!
//! 此前只有 MCP `dlq_list` / `dlq_requeue`，后台"需要注意"里的 DLQ 待办点过去却什么都
//! 没有——承诺跳转、落地没有。这里给 HTTP 端点，MCP 与 HTTP 共用同一份重投/丢弃逻辑。
//!
//! 两个终态动作：**重投**（payload 重入 outbox，适用瞬时故障：CH 短暂不可达）与
//! **丢弃**（标记已处理，适用毒消息：payload 本身坏的，重投只会再进 DLQ）。
//! 丢弃不删行——留着错误原文与谁处理的，审计要看得见"这笔账为什么没进统计"。

use super::query::Query;
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

#[derive(Deserialize)]
pub struct DlqQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// 缺省只看未处理；`all=true` 连已丢弃的一起列（审计回看）。
    #[serde(default)]
    pub all: Option<bool>,
    #[serde(default)]
    pub batch_id: Option<uuid::Uuid>,
}

/// GET /admin/dlq：死信列表，带 payload 摘要（这是哪笔账：请求 ID / 用户 / 模型 / 金额）。
/// 只看 id 与错误串没法判断"能不能重投"——看到是哪个用户哪个模型的账才敢下手。
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DlqQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let maximum = if q.batch_id.is_some() { 500 } else { 200 };
    let limit = q
        .limit
        .unwrap_or(if q.batch_id.is_some() { 500 } else { 50 })
        .clamp(1, maximum);
    let include_resolved = q.all == Some(true);
    let rows = sqlx::query!(
        r#"SELECT id, source, error, retry_count, status, created_at, resolved_at, resolved_by,
                  payload->>'request_id' AS "request_id?",
                  payload->>'user_id' AS "user_id?",
                  payload->>'model' AS "model?",
                  payload->>'amount_micro' AS "amount_micro?", ch_batch_id, event_key,
                  (SELECT event_count FROM billing_ch_batches b WHERE b.id=billing_dlq.ch_batch_id) AS "batch_size?"
           FROM billing_dlq
           WHERE ($1::boolean OR status = 0) AND ($3::uuid IS NULL OR ch_batch_id=$3)
           ORDER BY id DESC LIMIT $2"#,
        include_resolved,
        limit,
        q.batch_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let pending = pending_depth(&state.pg).await?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "source": r.source,
                "error": r.error,
                "retry_count": r.retry_count,
                // 0 待处理 / 2 已丢弃
                "status": r.status,
                "created_at": r.created_at.to_rfc3339(),
                "resolved_at": r.resolved_at.map(|t| t.to_rfc3339()),
                "resolved_by": r.resolved_by,
                "request_id": r.request_id,
                "user_id": r.user_id.and_then(|s| s.parse::<i64>().ok()),
                "model": r.model,
                "amount_micro": r.amount_micro.and_then(|s| s.parse::<i64>().ok()),
                "delivery_batch_id": r.ch_batch_id,
                "delivery_event_key": r.event_key,
                "delivery_batch_size":r.batch_size,
            })
        })
        .collect();
    Ok(Json(json!({ "pending": pending, "data": data })))
}

#[derive(Deserialize)]
pub struct IdsReq {
    pub ids: Vec<i64>,
}

/// POST /admin/dlq/requeue：payload 重入 outbox 并删除 DLQ 行（与 MCP 同一函数）。
pub async fn requeue_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<IdsReq>,
) -> Result<Json<Value>, AppError> {
    let actor = super::admin::guard(&state, &headers, permissions::BILLING_REFUND).await?;
    if req.ids.is_empty() {
        return Err(AppError::bad_request().with_param("ids"));
    }
    let outcome = requeue(&state.pg, &req.ids).await?;
    super::admin::audit(
        &state,
        &actor,
        "billing.dlq_requeue",
        "batch",
        json!({ "ids": req.ids, "requeued": outcome.count,
            "affected_ids":outcome.dlq_ids,"delivery_batch_ids":outcome.batch_ids }),
    )
    .await;
    Ok(Json(
        json!({ "requeued": outcome.count,"delivery_batch_ids":outcome.batch_ids }),
    ))
}

/// POST /admin/dlq/discard：标记已处理；冻结批次必须整体处理。
pub async fn discard_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<IdsReq>,
) -> Result<Json<Value>, AppError> {
    let actor = super::admin::guard(&state, &headers, permissions::BILLING_REFUND).await?;
    if req.ids.is_empty() {
        return Err(AppError::bad_request().with_param("ids"));
    }
    let discarded = sqlx::query!(
        r#"WITH locked_batches AS MATERIALIZED (
               SELECT id FROM billing_ch_batches WHERE status=2 AND id IN (
                   SELECT ch_batch_id FROM billing_dlq WHERE id=ANY($1) AND status=0
               ) ORDER BY id FOR UPDATE
           ), blocked AS (
               SELECT EXISTS(SELECT 1 FROM billing_dlq WHERE status=0
                   AND ch_batch_id IN (SELECT id FROM locked_batches) AND NOT(id=ANY($1))) AS partial
           ), upd AS (
               UPDATE billing_dlq SET status = 2, resolved_at = now(), resolved_by = $2
               WHERE status=0 AND id=ANY($1) AND NOT(SELECT partial FROM blocked)
               RETURNING 1
           ) SELECT COUNT(*)::bigint AS "c!",(SELECT partial FROM blocked) AS "partial!" FROM upd"#,
        &req.ids,
        actor.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if discarded.partial {
        return Err(AppError::bad_request().with_param("delivery_batch_members"));
    }
    let discarded = discarded.c;
    super::admin::audit(
        &state,
        &actor,
        "billing.dlq_discard",
        "batch",
        json!({ "ids": req.ids, "discarded": discarded }),
    )
    .await;
    Ok(Json(json!({ "discarded": discarded })))
}

/// 重投：冻结批次整体恢复原 token/行；旧 DLQ 仍重入 outbox。
/// 返回实际恢复的所有成员数，可能大于选择的条数。
/// MCP `dlq_requeue` 与 HTTP 共用——AI 与人执行的必须是同一个动作。
pub(super) struct Requeued {
    pub count: i64,
    pub dlq_ids: Vec<i64>,
    pub batch_ids: Vec<uuid::Uuid>,
}

pub(super) async fn requeue(pg: &PgPool, ids: &[i64]) -> Result<Requeued, AppError> {
    let n = sqlx::query!(
        r#"
        WITH locked_batches AS MATERIALIZED (
            SELECT id FROM billing_ch_batches WHERE status=2 AND id IN (
                SELECT ch_batch_id FROM billing_dlq WHERE id=ANY($1) AND status=0
            ) ORDER BY id FOR UPDATE
        ), moved AS (
            DELETE FROM billing_dlq WHERE status=0
                AND (id=ANY($1) OR ch_batch_id IN (SELECT id FROM locked_batches))
            RETURNING id,payload,ch_batch_id,event_key
        ), restored AS (
            UPDATE billing_ch_batches SET status=0,retry_count=0,next_retry_at=NULL
            WHERE id IN (SELECT ch_batch_id FROM moved) AND status=2 RETURNING id
        ), reset_outbox AS (
            UPDATE billing_outbox SET status=0,retry_count=0,next_retry_at=NULL
            WHERE ch_batch_id IN (SELECT id FROM restored) RETURNING id
        ), restored_relay AS (
            UPDATE billing_outbox o SET status=0,retry_count=0,next_retry_at=NULL,topic='billing.completed',payload=m.payload
            FROM moved m WHERE m.ch_batch_id IS NULL AND m.event_key='outbox:'||o.event_id::text
            RETURNING o.event_id
        ), ins AS (
            INSERT INTO billing_outbox (topic, payload)
            SELECT 'billing.completed', payload FROM moved m WHERE ch_batch_id IS NULL
                AND NOT EXISTS(SELECT 1 FROM billing_outbox o WHERE m.event_key='outbox:'||o.event_id::text)
            RETURNING 1
        )
        SELECT COUNT(*)::bigint AS "c!",
            COALESCE(array_agg(id),'{}'::bigint[]) AS "dlq_ids!",
            COALESCE(array_agg(DISTINCT ch_batch_id) FILTER (WHERE ch_batch_id IS NOT NULL),
                     '{}'::uuid[]) AS "batch_ids!" FROM moved
        "#,
        ids
    )
    .fetch_one(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(Requeued {
        count: n.c,
        dlq_ids: n.dlq_ids,
        batch_ids: n.batch_ids,
    })
}

/// Preview the atomic batch scope before MCP confirmation.
pub(super) async fn requeue_scope(pg: &PgPool, ids: &[i64]) -> Result<i64, AppError> {
    let n = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_dlq WHERE status=0 AND
           (id=ANY($1) OR ch_batch_id IN (
               SELECT ch_batch_id FROM billing_dlq WHERE id=ANY($1) AND status=0
           ))"#,
        ids
    )
    .fetch_one(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(n)
}

/// 未处理死信数——diagnose 与"需要注意"面板的口径。已丢弃的行不计，
/// 否则处理过的毒消息会让待办永远红。
pub(super) async fn pending_depth(pg: &PgPool) -> Result<i64, AppError> {
    let n =
        sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_dlq WHERE status = 0"#)
            .fetch_one(pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    Ok(n)
}
