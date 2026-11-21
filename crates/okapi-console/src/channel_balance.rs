//! 上游余额查询的控制台入口（IMPLEMENTATION §11.33）；探针、解析与请求在 `okapi_engine::channel_balance`，
//! 定时刷新的 worker 任务（§11.73）共用同一套。

use super::admin::{ensure_channel_owner, guard_scoped};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::permissions;
use okapi_engine::channel_balance::{BalanceError, Probe};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Path;
use serde_json::Value;

/// GET /admin/channels/{id}/balance：查上游余额并留痕（`ch:balance:<id>`，列表回填）。
/// 与测活 / 拉模型同门（`channel.write` + own 范围）；只读不审计。
pub async fn fetch_channel_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    let row = sqlx::query!(
        r#"
        SELECT c.provider, c.api_base, c.settings, ck.id AS key_id, ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        WHERE c.id = $1 AND c.deleted_at IS NULL
        ORDER BY ck.id
        LIMIT 1
        "#,
        channel_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            okapi_api::codes::NO_AVAILABLE_CHANNEL,
        )
    })?;
    let base = row.api_base.unwrap_or_default();
    let base = base.trim_end_matches('/');
    let Some(probe) = Probe::for_channel(&row.provider, base) else {
        return Err(AppError::bad_request().with_param("balance_unsupported"));
    };
    let credential =
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
    let outbound = okapi_providers::Outbound::from_settings(
        &row.settings,
        super::egress::key_proxy(&state, row.key_id).await?,
    );

    let balance = okapi_engine::channel_balance::fetch(&state, probe, base, &credential, &outbound)
        .await
        .map_err(balance_error)?;
    let result = okapi_engine::channel_balance::record(channel_id, probe, &balance);
    state
        .sched
        .channel_balance_record(channel_id, &result)
        .await;
    Ok(Json(result))
}

fn balance_error(error: BalanceError) -> AppError {
    match error {
        BalanceError::Upstream(param) => {
            AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                .with_param(param)
        }
        BalanceError::Shape => AppError::bad_request().with_param("balance_shape"),
    }
}
