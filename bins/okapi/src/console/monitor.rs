//! 运维监控的控制面出口（§11.43）：实时概况、24 小时趋势、告警日志。
//! 都是只读，但涉及主机与中间件内部状态，按 `settings.read` 把关（与系统设置同级）。

use super::query::Query;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use crate::ops::{host, logbuf, probes::Probes, sampler};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

fn probes(state: &AppState) -> Probes {
    Probes {
        pg: state.pg.clone(),
        redis: state.sched.client().clone(),
        ch: state.ch.clone(),
        nats: state.nats.clone(),
    }
}

/// GET /admin/monitor/overview：本进程所在机器的压力 + 各中间件占用（现查，约 0.5 秒）。
pub async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::SETTINGS_READ).await?;
    let probes = probes(&state);
    let (rates, middleware) = tokio::join!(
        host::rates_now(Duration::from_millis(500)),
        probes.snapshot()
    );
    let mut body = json!({
        "collected_at": chrono::Utc::now().to_rfc3339(),
        "node": &*state.node,
        "host": host::host(),
        "rates": rates,
    });
    if let (Some(dst), Value::Object(src)) = (body.as_object_mut(), middleware) {
        dst.extend(src);
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub hours: Option<i64>,
}

/// GET /admin/monitor/history?hours=1..24：worker 每分钟一个点。
pub async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::SETTINGS_READ).await?;
    let hours = q.hours.unwrap_or(6).clamp(1, 24);
    let points = sampler::load(state.sched.client(), hours * 60)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "ops history read failed");
            AppError::internal()
        })?;
    Ok(Json(
        json!({"hours": hours, "interval_secs": 60, "data": points}),
    ))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    /// `error` 只看错误；缺省 WARN + ERROR。
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /admin/monitor/logs：各进程汇总的 WARN / ERROR（新的在前）。
pub async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::SETTINGS_READ).await?;
    let (entries, source) = match logbuf::load(state.sched.client()).await {
        Ok(entries) => (entries, "shared"),
        Err(_) => (logbuf::recent(), "local"),
    };
    let only_error = q.level.as_deref() == Some("error");
    let needle =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
    let limit = q.limit.unwrap_or(200).clamp(1, 2000);
    let total = entries.len();
    let errors = entries.iter().filter(|e| e.level == "ERROR").count();
    let data: Vec<_> = entries
        .into_iter()
        .filter(|e| !only_error || e.level == "ERROR")
        .filter(|e| {
            needle.as_ref().is_none_or(|n| {
                e.message.to_lowercase().contains(n) || e.target.to_lowercase().contains(n)
            })
        })
        .take(limit)
        .collect();
    Ok(Json(json!({
        "source": source,
        "total": total,
        "errors": errors,
        "data": data,
    })))
}
