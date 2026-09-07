//! 负毛利熔断管理面（IMPLEMENTATION §11.34）：列出当前熔断 / 解除。
//! 状态在 Redis `mb:blocks`（契约见 `crate::margin`），配置在 `settings.margin_breaker`。

use super::admin::{audit, guard};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use crate::margin::{self, BlockEntry, BlockState, BreakerConfig};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use serde::Deserialize;
use serde_json::{Value, json};

/// GET /admin/margin-breaker：生效配置 + 全部条目（含 lifted，让人看见"谁解除的还在保护期"）。
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::CHANNEL_READ).await?;
    let config = BreakerConfig::from_setting(
        state
            .setting_cached(margin::SETTING_KEY)
            .await
            .as_ref()
            .as_ref(),
    );
    let mut entries: Vec<(String, i64, BlockEntry)> = state
        .sched
        .margin_blocks()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(field, entry)| {
            margin::parse_field(&field).map(|(g, id)| (g.to_owned(), id, entry))
        })
        .collect();
    entries.sort_by_key(|(_, _, e)| std::cmp::Reverse(e.since));

    let ids: Vec<i64> = entries.iter().map(|(_, id, _)| *id).collect();
    let names = sqlx::query!(r#"SELECT id, name FROM channels WHERE id = ANY($1)"#, &ids)
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let now = chrono::Utc::now().timestamp();
    let data: Vec<Value> = entries
        .into_iter()
        .map(|(group_code, channel_id, e)| {
            json!({
                "group_code": group_code,
                "channel_id": channel_id,
                "channel_name": names.iter().find(|n| n.id == channel_id).map(|n| n.name.clone()),
                "state": e.state,
                "active": e.blocks_at(now),
                "since": e.since,
                "until": e.until,
                "requests": e.requests,
                "amount_micro": e.amount_micro,
                "cost_micro": e.cost_micro,
                "margin_bp": e.margin_bp,
            })
        })
        .collect();
    Ok(Json(json!({ "config": config, "data": data })))
}

#[derive(Deserialize)]
pub struct LiftReq {
    pub group_code: String,
    pub channel_id: i64,
}

/// POST /admin/margin-breaker/lift：解除一对，并在 `lift_secs` 内免评估——
/// 窗口里的旧亏损还在，不给这段时间它会在下一轮立刻被重新熔断。
pub async fn lift(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LiftReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let group = req.group_code.trim();
    if group.is_empty() || group.contains('|') {
        return Err(AppError::bad_request().with_param("group_code"));
    }
    let field = margin::field(group, req.channel_id);
    let existing = state
        .sched
        .margin_blocks()
        .await
        .and_then(|mut m| m.remove(&field))
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    let config = BreakerConfig::from_setting(
        state
            .setting_cached(margin::SETTING_KEY)
            .await
            .as_ref()
            .as_ref(),
    );
    let now = chrono::Utc::now().timestamp();
    let entry = BlockEntry {
        state: BlockState::Lifted,
        since: now,
        until: now + config.lift_secs,
        ..existing
    };
    state
        .sched
        .margin_block_set(&field, &entry)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "margin lift 写入失败");
            AppError::internal()
        })?;
    // 同进程立即生效；其它副本靠 10s TTL 收敛
    state.margin_cache.invalidate_all();
    audit(
        &state,
        &actor,
        "margin.lift",
        &field,
        json!({ "group_code": group, "channel_id": req.channel_id, "until": entry.until }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "until": entry.until })))
}
