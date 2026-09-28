//! Recorded subscription identity remains stable after clock expiry until an
//! explicit roll/end replaces it. Redis admission separately checks its deadline.
use crate::{LedgerError, Pool};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

pub(crate) struct Window {
    pub token: String,
    pub until: i64,
}

pub(crate) async fn current(
    connection: &mut PgConnection,
    user_id: i64,
) -> Result<Option<Window>, LedgerError> {
    let row = sqlx::query!(
        "SELECT id,window_start,LEAST(window_end,expires_at) AS until_at FROM user_subscriptions WHERE user_id=$1 AND status=1 ORDER BY id DESC LIMIT 1",
        user_id
    ).fetch_optional(connection).await?;
    Ok(row.map(|r| Window {
        token: format!("{}:{}", r.id, r.window_start.timestamp_micros()),
        until: if r.window_start <= Utc::now() {
            r.until_at
                .map_or(0, |until: DateTime<Utc>| until.timestamp())
        } else {
            0
        },
    }))
}

pub(crate) async fn replaced(
    connection: &mut PgConnection,
    user_id: i64,
    pool: Pool,
    source: Option<&str>,
) -> Result<bool, LedgerError> {
    let Some(source) = source else {
        return Ok(false);
    };
    if pool != Pool::Subscription
        || source.is_empty()
        || source.len() > 128
        || !source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-:._".contains(&b))
    {
        return Err(LedgerError::InvalidSettlement);
    }
    Ok(current(connection, user_id)
        .await?
        .as_ref()
        .map(|w| w.token.as_str())
        != Some(source))
}

pub(crate) async fn expired_refund(
    connection: &mut PgConnection,
    user_id: i64,
    pool: Pool,
    source: Option<&str>,
) -> Result<bool, LedgerError> {
    if replaced(connection, user_id, pool, source).await? {
        return Ok(true);
    }
    if source.is_none() {
        return Ok(false);
    }
    Ok(current(connection, user_id)
        .await?
        .is_none_or(|window| Utc::now().timestamp() >= window.until))
}

/// Revise an earlier expiry without granting current-window spendable credit.
/// Positive delta offsets a late old-window consumption; negative expires an
/// administrator's refund of an already ended window. The bill stays exact.
pub(crate) async fn revise_expiry(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    request_id: Uuid,
    delta: i64,
    source: Option<&str>,
    reason: &str,
) -> Result<(), LedgerError> {
    sqlx::query!("INSERT INTO billing_events(user_id,request_id,event_type,delta_micro,payload,actor,pool) VALUES($1,$2,'sub_expire',$3,$4,'system:window',1)",
        user_id, request_id, delta, serde_json::json!({"reason":reason,"source_window":source}))
        .execute(&mut **tx).await?;
    Ok(())
}
