use super::{FrozenReserve, Hold, MAX_ACTIVE, MAXIMUM_MICROS, UserGuard, hot::HotHold};
use crate::LedgerError;
use sqlx::Acquire;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct WindowRow {
    id: i64,
    window_start: chrono::DateTime<chrono::Utc>,
    until_at: chrono::DateTime<chrono::Utc>,
}
pub(super) async fn window(
    guard: &mut UserGuard,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<(String, i64)>, LedgerError> {
    let row: Option<WindowRow> = sqlx::query_as("SELECT id,window_start,LEAST(window_end,expires_at) AS until_at FROM user_subscriptions WHERE user_id=$1 AND status=1 AND window_start<=$2 AND window_end>$2 AND expires_at>$2 ORDER BY id DESC LIMIT 1")
        .bind(guard.user_id).bind(now).fetch_optional(guard.connection()?).await?;
    Ok(row.map(|r| {
        (
            format!("{}:{}", r.id, r.window_start.timestamp_micros()),
            r.until_at.timestamp(),
        )
    }))
}

pub(super) async fn get(guard: &mut UserGuard, id: Uuid) -> Result<Hold, LedgerError> {
    sqlx::query_as("SELECT * FROM balance_holds WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(guard.user_id)
        .fetch_optional(guard.connection()?)
        .await?
        .ok_or(LedgerError::HoldConflict)
}

pub(super) async fn concurrency(guard: &mut UserGuard, key_id: i64) -> Result<i32, LedgerError> {
    let cap: Option<i32> =
        sqlx::query_scalar("SELECT max_concurrency FROM api_keys WHERE id=$1 AND user_id=$2")
            .bind(key_id)
            .bind(guard.user_id)
            .fetch_one(guard.connection()?)
            .await?;
    Ok(cap.unwrap_or(0))
}

pub(super) async fn intent(
    guard: &mut UserGuard,
    request: &FrozenReserve<'_>,
) -> Result<(Hold, bool), LedgerError> {
    if request.user_id != guard.user_id
        || request.id.is_nil()
        || request.user_id <= 0
        || request.api_key_id <= 0
        || !(0..=MAXIMUM_MICROS).contains(&request.maximum.as_micros())
        || request.model.is_empty()
        || request.model.len() > 256
        || request.model.chars().any(char::is_control)
        || request.request_hash.len() != 64
        || !request.request_hash.bytes().all(|v| v.is_ascii_hexdigit())
    {
        return Err(LedgerError::InvalidHold("request"));
    }
    let pricing = request.pricing;
    if !pricing.is_object()
        || pricing
            .get("group")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || pricing
            .get("epoch")
            .and_then(serde_json::Value::as_i64)
            .is_none()
        || pricing
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .is_none()
    {
        return Err(LedgerError::InvalidHold("pricing"));
    }
    if pricing.to_string().len() > 65_536 {
        return Err(LedgerError::InvalidHold("pricing_size"));
    }
    let mut tx = guard.connection()?.begin().await?;
    let existing: Option<Hold> = sqlx::query_as("SELECT * FROM balance_holds WHERE id=$1")
        .bind(request.id)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(hold) = existing {
        if hold.user_id != request.user_id
            || hold.api_key_id != request.api_key_id
            || hold.model_name != request.model
            || hold.request_hash != request.request_hash
            || hold.maximum_micro != request.maximum.as_micros()
            || &hold.pricing_snapshot != pricing
        {
            return Err(LedgerError::HoldConflict);
        }
        tx.commit().await?;
        return Ok((hold, false));
    }
    let owner: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE id=$1 AND user_id=$2 AND status=1 AND deleted_at IS NULL AND (expires_at IS NULL OR expires_at>now()))")
        .bind(request.api_key_id).bind(request.user_id).fetch_one(&mut *tx).await?;
    if !owner {
        return Err(LedgerError::HoldConflict);
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM balance_holds WHERE user_id=$1 AND state<>'closed'",
    )
    .bind(request.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if count >= MAX_ACTIVE {
        return Err(LedgerError::HoldCapacity);
    }
    let hold: Option<Hold> = sqlx::query_as("INSERT INTO balance_holds(id,user_id,api_key_id,model_name,request_hash,maximum_micro,pricing_snapshot) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(id) DO NOTHING RETURNING *")
        .bind(request.id).bind(request.user_id).bind(request.api_key_id).bind(request.model)
        .bind(request.request_hash).bind(request.maximum.as_micros()).bind(pricing)
        .fetch_optional(&mut *tx).await?;
    let hold = hold.ok_or(LedgerError::HoldConflict)?;
    // This intent must survive cancellation of the subsequent Redis request.
    tx.commit().await?;
    Ok((hold, true))
}

pub(super) async fn held(
    guard: &mut UserGuard,
    id: Uuid,
    receipt: &HotHold,
) -> Result<Hold, LedgerError> {
    let user_id = guard.user_id;
    let mut tx = guard.connection()?.begin().await?;
    let hold: Hold = sqlx::query_as("UPDATE balance_holds SET state='held',pool=$3,source_window=$4,updated_at=now() WHERE id=$1 AND user_id=$2 AND state='pending' RETURNING *")
        .bind(id).bind(user_id).bind(receipt.pool)
        .bind((receipt.pool == 1).then_some(receipt.epoch.as_str()))
        .fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO billing_events(user_id,request_id,event_type,delta_micro,payload,actor,pool) VALUES($1,$2,'reserve',0,$3,'system:batch',$4)")
        .bind(user_id).bind(id).bind(serde_json::json!({"hold_micro":hold.maximum_micro,"model":hold.model_name,"source_window":hold.source_window}))
        .bind(receipt.pool).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(hold)
}

pub(super) async fn closing(guard: &mut UserGuard) -> Result<Vec<Hold>, LedgerError> {
    Ok(sqlx::query_as(
        "SELECT * FROM balance_holds WHERE user_id=$1 AND state='closing' ORDER BY created_at,id",
    )
    .bind(guard.user_id)
    .fetch_all(guard.connection()?)
    .await?)
}
pub(super) async fn closed(guard: &mut UserGuard, id: Uuid) -> Result<(), LedgerError> {
    sqlx::query("UPDATE balance_holds SET state='closed',updated_at=now() WHERE id=$1 AND user_id=$2 AND state='closing'")
        .bind(id).bind(guard.user_id).execute(guard.connection()?).await?;
    Ok(())
}
