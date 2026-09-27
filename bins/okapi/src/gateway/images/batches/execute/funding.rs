use super::{AppError, AppState, Batch, Step, map_store, store};
use okapi_api::codes;
use okapi_domain::Money;
use okapi_ledger::holds::{self, Admission, FrozenReserve};

pub(super) async fn fund(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
) -> Result<Step, AppError> {
    if stop_before_submit(state, row, lease).await? {
        return Ok(Step::Retry(0));
    }
    state.check_settle_backlog()?;
    let admission = holds::reserve_frozen(
        &state.pg,
        &state.ledger,
        FrozenReserve {
            id: row.id,
            user_id: row.user_id,
            api_key_id: row.api_key_id,
            model: &row.model_name,
            request_hash: &row.request_hash,
            maximum: Money::from_micros(row.maximum_micro),
            pricing: &row.pricing_snapshot,
        },
        chrono::Utc::now(),
    )
    .await?;
    match admission {
        Admission::ConcurrencyLimited => return Ok(Step::Retry(3)),
        Admission::Held { .. } => {
            store::prepare(&state.pg, lease).await.map_err(map_store)?;
        }
        Admission::Insufficient { .. } => {
            store::abort_before_submission(&state.pg, lease, "insufficient_quota")
                .await
                .map_err(map_store)?;
        }
        Admission::Closed(_) => {
            store::abort_before_submission(&state.pg, lease, "batch_hold_closed")
                .await
                .map_err(map_store)?;
        }
    }
    Ok(Step::Retry(0))
}
pub(super) async fn stop_before_submit(
    state: &AppState,
    row: &Batch,
    lease: store::Lease,
) -> Result<bool, AppError> {
    let current = store::owned(&state.pg, row.id, row.user_id, row.api_key_id)
        .await
        .map_err(map_store)?
        .ok_or_else(AppError::internal)?;
    let reason = if current.cancel_requested {
        Some("batch_cancelled".to_owned())
    } else {
        match permissions(state, row).await {
            Ok(()) => None,
            Err(error) if error.status.is_client_error() => Some(error.code),
            Err(error) => return Err(error),
        }
    };
    if let Some(reason) = reason {
        store::abort_before_submission(&state.pg, lease, &reason)
            .await
            .map_err(map_store)?;
        return Ok(true);
    }
    Ok(false)
}
async fn permissions(state: &AppState, row: &Batch) -> Result<(), AppError> {
    let key_hash: Option<String> = sqlx::query_scalar(
        "SELECT key_hash FROM api_keys WHERE id=$1 AND user_id=$2 AND deleted_at IS NULL",
    )
    .bind(row.api_key_id)
    .bind(row.user_id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let hash = key_hash.ok_or_else(|| AppError::unauthorized(codes::KEY_DISABLED))?;
    let key = okapi_store::auth::find_key_by_hash(&state.pg, &hash)
        .await?
        .filter(|k| k.is_usable(chrono::Utc::now()))
        .ok_or_else(|| AppError::unauthorized(codes::KEY_DISABLED))?;
    if row.member_user_id != key.member_user_id {
        return Err(
            AppError::new(axum::http::StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("batch_member_changed"),
        );
    }
    crate::gateway::auth::check_member_limit(state, &key).await?;
    if !key.allows_ip(row.client_ip.as_ref().and_then(|v| v.parse().ok())) {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            codes::IP_NOT_ALLOWED,
        ));
    }
    if !key.allows_model(&row.model_name) {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            codes::MODEL_NOT_ALLOWED,
        ));
    }
    let active: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM models WHERE model_name=$1 AND status=1)")
            .bind(&row.model_name)
            .fetch_one(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    if !active {
        return Err(AppError::new(
            axum::http::StatusCode::NOT_FOUND,
            codes::MODEL_NOT_FOUND,
        ));
    }
    let mut candidates = okapi_store::channels::candidates_for_model(
        &state.pg,
        &row.model_name,
        &key.pool_chain(),
        state.master_key.as_deref(),
    )
    .await?;
    state
        .retain_margin_ok(&key.group_code, &mut candidates)
        .await;
    let candidate = candidates.iter().find(|c| {
        c.channel_id == row.channel_id
            && c.channel_key_id == row.channel_key_id
            && super::super::binding::eligible(c, Some(&row.provider))
    });
    let Some(candidate) = candidate else {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            codes::MODEL_NOT_ALLOWED,
        ));
    };
    if !super::super::budget_available(state, candidate).await {
        return Err(AppError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            codes::RATE_LIMITED,
        )
        .with_param("channel_daily_spend"));
    }
    Ok(())
}
