//! User-owned billing logs and full-filter aggregates share one SQL predicate.
//! Money comes from PostgreSQL, never from the currently loaded page or CH.

use super::{LogWindow, LogsQuery};
use crate::console::query::Query;
use crate::gateway::{auth::authenticate, error::AppError, state::AppState};
use axum::{Json, extract::State, http::HeaderMap};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};

fn filtered(
    sql: &'static str,
    q: &LogsQuery,
    window: Option<&LogWindow>,
    user_id: i64,
    key_id: i64,
) -> Result<QueryBuilder<Postgres>, AppError> {
    if q.api_key_id.is_some_and(|id| id <= 0) {
        return Err(AppError::bad_request().with_param("api_key_id"));
    }
    let mut query = QueryBuilder::new(sql);
    query.push(" FROM billing_records b LEFT JOIN api_keys k ON k.id = b.api_key_id AND k.user_id = b.user_id WHERE b.user_id = ").push_bind(user_id);
    if q.scope.as_deref() != Some("user") {
        query.push(" AND b.api_key_id = ").push_bind(key_id);
    }
    if let Some(id) = q.api_key_id {
        query.push(" AND b.api_key_id = ").push_bind(id);
    }
    if let Some(id) = q.request_id {
        query.push(" AND b.request_id = ").push_bind(id);
    }
    if let Some(model) = q.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        query.push(" AND b.model_name = ").push_bind(model);
    }
    if q.errors_only == Some(true) {
        query.push(" AND b.status = 40");
    }
    if let Some(window) = window {
        query
            .push(" AND b.created_at >= (")
            .push_bind(window.start)
            .push("::date::timestamp AT TIME ZONE ")
            .push_bind(&window.timezone)
            .push(")");
        query
            .push(" AND b.created_at < ((")
            .push_bind(window.end)
            .push("::date + 1)::timestamp AT TIME ZONE ")
            .push_bind(&window.timezone)
            .push(")");
    }
    Ok(query)
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let window = q.window(&state.pg).await?;
    let mut query = filtered(
        r"SELECT jsonb_build_object(
            'id', b.id, 'request_id', b.request_id, 'model', b.model_name,
            'log_type', b.log_type, 'status', b.status,
            'api_key_id', b.api_key_id, 'key_name', COALESCE(k.name, ''),
            'usage', jsonb_build_object(
                'prompt_tokens', b.prompt_tokens, 'cached_tokens', b.cached_tokens,
                'completion_tokens', b.completion_tokens, 'reasoning_tokens', b.reasoning_tokens,
                'upstream_usage', b.usage_details->'tokens'->'upstream_usage',
                'prompt_source', COALESCE(b.usage_details->>'prompt_source', 'unknown'),
                'completion_source', COALESCE(b.usage_details->>'completion_source', 'unknown'),
                'cache_read_reported', COALESCE((b.usage_details->'tokens'->>'cache_read_reported')::boolean, NULLIF(b.cached_tokens > 0, false)),
                'cache_write_reported', (b.usage_details->'tokens'->>'cache_write_reported')::boolean,
                'cache_write_tokens', b.usage_details->'tokens'->'cache_write_tokens',
                'audio_prompt_tokens', b.usage_details->'tokens'->'audio_prompt_tokens',
                'image_prompt_tokens', b.usage_details->'tokens'->'image_prompt_tokens',
                'audio_completion_tokens', b.usage_details->'tokens'->'audio_completion_tokens',
                'image_completion_tokens', b.usage_details->'tokens'->'image_completion_tokens',
                'cache_read_modalities', b.usage_details->'tokens'->'cache_read_modalities',
                'cache_write_modalities', b.usage_details->'tokens'->'cache_write_modalities'
            ),
            'usage_details_recorded', b.usage_details IS NOT NULL,
            'requested_model', NULLIF(b.usage_details->>'requested_model', ''),
            'endpoint', NULLIF(b.usage_details->>'endpoint', ''),
            'pool', b.pool, 'amount_micro', b.amount_micro,
            'net_amount_micro', CASE WHEN b.status = 20 THEN b.amount_micro ELSE 0 END,
            'original_amount_micro', b.original_amount_micro, 'discount_micro', b.discount_micro,
            'pricing_snapshot', b.pricing_snapshot, 'error_code', b.error_code,
            'latency_ms', b.latency_ms, 'ttft_ms', b.ttft_ms,
            'is_stream', b.is_stream, 'created_at', b.created_at
        )",
        &q,
        window.as_ref(),
        key.user_id,
        key.key_id,
    )?;
    if let Some(before) = q.before {
        query.push(" AND b.id < ").push_bind(before);
    }
    let limit = q.limit.clamp(1, 200);
    // Fetch one extra row so exactly-full final pages do not advertise another page.
    query
        .push(" ORDER BY b.id DESC LIMIT ")
        .push_bind(limit + 1);
    let mut data: Vec<Value> = query
        .build_query_scalar()
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let has_more = data.len() > usize::try_from(limit).unwrap_or(200);
    if has_more {
        data.pop();
    }
    let next_before = if has_more {
        data.last().and_then(|r| r["id"].as_i64())
    } else {
        None
    };
    Ok(Json(json!({
        "scope": if q.scope.as_deref() == Some("user") { "user" } else { "key" },
        "window": window.map(|w| json!({ "start_date": w.start, "end_date": w.end, "timezone": w.timezone })),
        "data": data, "next_before": next_before,
    })))
}

pub async fn stat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let window = q.window(&state.pg).await?;
    // Deliberately ignore cursor/limit: the summary describes ALL matching ledger rows.
    // Refunds change the original row to 30, retaining its original amount and usage.
    let mut query = filtered(
        r"SELECT jsonb_build_object(
            'records', COUNT(*),
            'settled', COUNT(*) FILTER (WHERE b.status = 20),
            'failed', COUNT(*) FILTER (WHERE b.status = 40),
            'refunded', COUNT(*) FILTER (WHERE b.status = 30),
            'pending', COUNT(*) FILTER (WHERE b.status = 10),
            'amount_micro', COALESCE(SUM(b.amount_micro) FILTER (WHERE b.status = 20), 0),
            'refunded_amount_micro', COALESCE(SUM(b.amount_micro) FILTER (WHERE b.status = 30), 0),
            'prompt_tokens', COALESCE(SUM(b.prompt_tokens::bigint), 0),
            'completion_tokens', COALESCE(SUM(b.completion_tokens::bigint), 0),
            'cached_tokens', COALESCE(SUM(b.cached_tokens::bigint), 0),
            'cache_read_samples', COUNT(*) FILTER (WHERE COALESCE((b.usage_details->'tokens'->>'cache_read_reported')::boolean, b.cached_tokens > 0)),
            'avg_latency_ms', FLOOR(AVG(b.latency_ms) FILTER (WHERE b.status IN (20,30,40) AND b.latency_ms >= 0)),
            'latency_samples', COUNT(*) FILTER (WHERE b.status IN (20,30,40) AND b.latency_ms >= 0),
            'avg_ttft_ms', FLOOR(AVG(b.ttft_ms) FILTER (WHERE b.status IN (20,30,40) AND b.is_stream AND b.ttft_ms >= 0)),
            'ttft_samples', COUNT(*) FILTER (WHERE b.status IN (20,30,40) AND b.is_stream AND b.ttft_ms >= 0)
        )",
        &q,
        window.as_ref(),
        key.user_id,
        key.key_id,
    )?;
    let result: Value = query
        .build_query_scalar()
        .fetch_one(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(Json(result))
}
