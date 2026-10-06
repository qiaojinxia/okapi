//! Native tool analytics share the ordinary billing read and owner boundaries.
use super::query::Query;
use super::usage_details::CalendarWindow;
use crate::gateway::{error::AppError, state::AppState};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use okapi_api::{codes, permissions};
use okapi_store::ch::server_tools::{
    self, Breakdown, Filters, Granularity, ModelSource, StatisticsQuery,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Default, Deserialize)]
pub struct ToolQuery {
    #[serde(default)]
    days: Option<u32>,
    start_date: Option<String>,
    end_date: Option<String>,
    #[serde(default)]
    granularity: Granularity,
    #[serde(default)]
    model_source: ModelSource,
    #[serde(default)]
    by: Breakdown,
    limit: Option<u32>,
    #[serde(default)]
    offset: u32,
    scope: Option<String>,
    user_id: Option<i64>,
    api_key_id: Option<i64>,
    channel_id: Option<i64>,
    model: Option<String>,
    group: Option<String>,
    endpoint: Option<String>,
    upstream_endpoint: Option<String>,
    node: Option<String>,
    stream: Option<bool>,
    request_type: Option<String>,
    billing_type: Option<String>,
}

fn validate(q: &ToolQuery) -> Result<(), AppError> {
    for (name, value) in [
        ("user_id", q.user_id),
        ("api_key_id", q.api_key_id),
        ("channel_id", q.channel_id),
    ] {
        if value.is_some_and(|v| v < 0) {
            return Err(AppError::bad_request().with_param(name));
        }
    }
    for (name, value) in [
        ("model", &q.model),
        ("group", &q.group),
        ("endpoint", &q.endpoint),
        ("upstream_endpoint", &q.upstream_endpoint),
        ("node", &q.node),
        ("request_type", &q.request_type),
        ("billing_type", &q.billing_type),
    ] {
        if value.as_ref().is_some_and(|v| v.len() > 512) {
            return Err(AppError::bad_request().with_param(name));
        }
    }
    if q.request_type
        .as_deref()
        .is_some_and(|v| !matches!(v, "stream" | "non_stream" | "websocket"))
    {
        return Err(AppError::bad_request().with_param("request_type"));
    }
    Ok(())
}

async fn response(state: &AppState, q: ToolQuery) -> Result<Json<Value>, AppError> {
    validate(&q)?;
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))?;
    let window = CalendarWindow::read(
        ch,
        q.days.unwrap_or(7),
        q.start_date.as_deref(),
        q.end_date.as_deref(),
    )
    .await?;
    if matches!(q.granularity, Granularity::Hour) && window.days() > 31 {
        return Err(AppError::bad_request().with_param("granularity"));
    }
    let mut result = server_tools::read(
        ch,
        &StatisticsQuery {
            start: window.start,
            end: window.end,
            filters: Filters {
                user_id: q.user_id,
                api_key_id: q.api_key_id,
                channel_id: q.channel_id,
                model: q.model,
                group: q.group,
                endpoint: q.endpoint,
                upstream_endpoint: q.upstream_endpoint,
                node: q.node,
                stream: q.stream,
                request_type: q.request_type,
                billing_type: q.billing_type,
            },
            model_source: q.model_source,
            granularity: q.granularity,
            by: q.by,
            limit: q.limit.unwrap_or(20),
            offset: q.offset,
        },
    )
    .await?;
    result["window"] = window.json();
    Ok(Json(result))
}

pub async fn admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ToolQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    if q.scope.is_some() {
        return Err(AppError::bad_request().with_param("scope"));
    }
    response(&state, q).await
}

pub async fn mine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(mut q): Query<ToolQuery>,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    let user_scope = match q.scope.as_deref() {
        None | Some("key") => false,
        Some("user") => true,
        Some(_) => return Err(AppError::bad_request().with_param("scope")),
    };
    if q.user_id.is_some_and(|v| v != key.user_id)
        || (!user_scope && q.api_key_id.is_some_and(|v| v != key.key_id))
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::PERMISSION_DENIED,
        ));
    }
    q.user_id = Some(key.user_id);
    if !user_scope {
        q.api_key_id = Some(key.key_id);
    }
    response(&state, q).await
}
