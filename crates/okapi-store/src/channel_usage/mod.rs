//! Durable channel admission counters, with billing records as the usage/cost authority.
use crate::{StoreError, timezone};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct TokenSnapshot {
    pub tokens: i64,
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
}

/// Lifetime totals survive retention; calendar windows use retained bills and
/// receipts under the same retention lock. No request/token estimation is used.
pub async fn token_snapshot(
    pool: &PgPool,
    channel: i64,
    period: &str,
) -> Result<TokenSnapshot, StoreError> {
    if period == "total" {
        let tokens = sqlx::query_scalar::<_, i64>("SELECT COALESCE((SELECT LEAST(tokens,9223372036854775807)::bigint FROM channel_token_totals WHERE channel_id=$1),0)")
            .bind(channel).fetch_one(pool).await?;
        return Ok(TokenSnapshot {
            tokens,
            window_start: None,
            window_end: None,
        });
    }
    let interval = match period {
        "day" => "1 day",
        "week" => "1 week",
        _ => return Err(StoreError::InvalidData("token_period")),
    };
    let zone = timezone::machine_timezone()?;
    let mut tx = crate::history::read(pool).await?;
    let usage = sqlx::query_as::<_, TokenSnapshot>(r"
        WITH bounds AS (SELECT date_trunc($2,now() AT TIME ZONE $3) AS start),
        period_window AS (SELECT start AT TIME ZONE $3 AS window_start,
            (start+$4::text::interval) AT TIME ZONE $3 AS window_end FROM bounds),
        facts AS (
            SELECT greatest(prompt_tokens,0)::bigint + greatest(completion_tokens,0)::bigint AS tokens
              FROM billing_records,period_window WHERE channel_id=$1 AND log_type IN (2,5)
                AND created_at>=window_start AND created_at<window_end
            UNION ALL
            SELECT channel_receipt_tokens(usage_details) FROM billing_record_receipts,period_window
              WHERE channel_id=$1 AND created_at>=window_start AND created_at<window_end)
        SELECT LEAST(COALESCE((SELECT SUM(tokens) FROM facts),0),9223372036854775807)::bigint AS tokens,
            window_start,window_end FROM period_window
    ").bind(channel).bind(period).bind(zone).bind(interval).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(usage)
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Snapshot {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub requests: i64,
    pub tokens: i64,
    pub cost_micro: i64,
    pub unknown_cost_requests: i64,
}

async fn read(
    tx: &mut Transaction<'_, Postgres>,
    channel: i64,
    period: &str,
) -> Result<Snapshot, StoreError> {
    let interval = match period {
        "hour" => "1 hour",
        "day" => "1 day",
        "week" => "1 week",
        "month" => "1 month",
        _ => return Err(StoreError::InvalidData("usage_period")),
    };
    let zone = timezone::machine_timezone()?;
    Ok(sqlx::query_as::<_, Snapshot>(r"
        WITH bounds AS (SELECT date_trunc($2,now() AT TIME ZONE $3) AS start,
                               date_trunc($2,now(),$3) AS window_start),
        period_window AS (SELECT window_start,
                  CASE WHEN $2='hour' THEN window_start + interval '1 hour'
                       ELSE (start + $4::text::interval) AT TIME ZONE $3 END AS window_end FROM bounds),
        usage AS (SELECT count(*)::bigint AS completed,
             COALESCE(sum(greatest(prompt_tokens,0)::bigint + greatest(completion_tokens,0)::bigint),0)::bigint AS tokens,
             COALESCE(sum(greatest(upstream_cost_micro,0)),0)::bigint AS cost_micro,
             count(*) FILTER (WHERE upstream_cost_micro IS NULL AND log_type=2)::bigint AS unknown_cost_requests
          FROM billing_records, period_window WHERE channel_id=$1 AND log_type IN (2,5)
             AND created_at >= window_start AND created_at < window_end)
        SELECT period_window.window_start,period_window.window_end,COALESCE(w.requests,usage.completed)::bigint AS requests,
               tokens,cost_micro,unknown_cost_requests FROM period_window CROSS JOIN usage
        LEFT JOIN channel_usage_windows w ON w.channel_id=$1 AND w.period=$2 AND w.window_start=period_window.window_start
    ").bind(channel).bind(period).bind(zone).bind(interval).fetch_one(&mut **tx).await?)
}

pub async fn snapshot(pool: &PgPool, channel: i64, period: &str) -> Result<Snapshot, StoreError> {
    let mut tx = pool.begin().await?;
    let value = read(&mut tx, channel, period).await?;
    tx.commit().await?;
    Ok(value)
}

/// The request cap is atomic across keys/processes. Actual token/cost limits stop NEW
/// attempts once settled usage reaches the cap; already running requests may exceed it.
pub async fn admit(
    pool: &PgPool,
    channel: i64,
    period: &str,
    requests: Option<i64>,
    tokens: Option<i64>,
    cost: Option<i64>,
) -> Result<bool, StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(18991001, hashtext($1))")
        .bind(channel.to_string())
        .execute(&mut *tx)
        .await?;
    let usage = read(&mut tx, channel, period).await?;
    if requests.is_some_and(|cap| usage.requests >= cap)
        || tokens.is_some_and(|cap| usage.tokens >= cap)
        || cost.is_some_and(|cap| usage.unknown_cost_requests > 0 || usage.cost_micro >= cap)
    {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query(
        r"INSERT INTO channel_usage_windows(channel_id,period,window_start,window_end,requests)
                  VALUES($1,$2,$3,$4,$5) ON CONFLICT(channel_id,period,window_start)
                  DO UPDATE SET requests=channel_usage_windows.requests+1",
    )
    .bind(channel)
    .bind(period)
    .bind(usage.window_start)
    .bind(usage.window_end)
    .bind(usage.requests.saturating_add(1))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn prune(pool: &PgPool) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM channel_usage_windows WHERE ctid IN (SELECT ctid FROM channel_usage_windows WHERE window_end < now() - interval '45 days' LIMIT 1000)")
        .execute(pool)
        .await?;
    Ok(())
}
