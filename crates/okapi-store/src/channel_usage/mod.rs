//! Durable channel admission counters, with billing records as the usage/cost authority.
use crate::{StoreError, timezone};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};

/// What one channel has on record for one period: input plus output tokens and the
/// upstream cost of its settled requests. A request whose cost is unknown adds no cost.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct LocalUsage {
    pub tokens: i64,
    pub cost_micro: i64,
    /// `None` for `total`, which has no window and never resets.
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
}

/// Lifetime totals survive retention; calendar windows sum materialized 15-minute
/// UTC buckets (`channel_usage_buckets`, aligned with every IANA timezone boundary)
/// without acquiring retention locks or scanning `billing_records` / `billing_record_receipts`.
pub async fn local_usage(
    pool: &PgPool,
    channel: i64,
    period: &str,
) -> Result<LocalUsage, StoreError> {
    if period == "total" {
        // An aggregate without GROUP BY yields its one row for a channel that has no totals yet.
        return Ok(sqlx::query_as::<_, LocalUsage>(
            r"SELECT COALESCE(MAX(LEAST(tokens,9223372036854775807)),0)::bigint AS tokens,
                     COALESCE(MAX(LEAST(cost_micro,9223372036854775807)),0)::bigint AS cost_micro,
                     NULL::timestamptz AS window_start, NULL::timestamptz AS window_end
                FROM channel_token_totals WHERE channel_id=$1",
        )
        .bind(channel)
        .fetch_one(pool)
        .await?);
    }
    let interval = match period {
        "day" => "1 day",
        "week" => "1 week",
        "month" => "1 month",
        _ => return Err(StoreError::InvalidData("usage_period")),
    };
    let zone = timezone::reporting_timezone()?;
    Ok(sqlx::query_as::<_, LocalUsage>(
        r"
        WITH bounds AS (SELECT date_trunc($2,now() AT TIME ZONE $3) AS start),
        period_window AS (
            SELECT start AT TIME ZONE $3 AS window_start,
                   (start+$4::text::interval) AT TIME ZONE $3 AS window_end
              FROM bounds
        )
        SELECT LEAST(COALESCE(SUM(b.tokens),0),9223372036854775807)::bigint AS tokens,
               LEAST(COALESCE(SUM(b.cost_micro),0),9223372036854775807)::bigint AS cost_micro,
               period_window.window_start,
               period_window.window_end
          FROM period_window
          LEFT JOIN channel_usage_buckets b
            ON b.channel_id=$1
           AND b.bucket_start>=period_window.window_start
           AND b.bucket_start<period_window.window_end
         GROUP BY period_window.window_start, period_window.window_end
    ",
    )
    .bind(channel)
    .bind(period)
    .bind(zone)
    .bind(interval)
    .fetch_one(pool)
    .await?)
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
    let zone = timezone::reporting_timezone()?;
    Ok(sqlx::query_as::<_, Snapshot>(r"
        WITH bounds AS (SELECT date_trunc($2,now() AT TIME ZONE $3) AS start,
                               date_trunc($2,now(),$3) AS window_start),
        period_window AS (SELECT window_start,
                  CASE WHEN $2='hour' THEN window_start + interval '1 hour'
                       ELSE (start + $4::text::interval) AT TIME ZONE $3 END AS window_end FROM bounds),
        usage AS (
            SELECT COALESCE(SUM(requests),0)::bigint AS completed,
                   LEAST(COALESCE(SUM(tokens),0),9223372036854775807)::bigint AS tokens,
                   LEAST(COALESCE(SUM(cost_micro),0),9223372036854775807)::bigint AS cost_micro,
                   COALESCE(SUM(unknown_cost_requests),0)::bigint AS unknown_cost_requests
              FROM channel_usage_buckets, period_window
             WHERE channel_id=$1
               AND bucket_start >= window_start
               AND bucket_start < window_end
        )
        SELECT period_window.window_start,period_window.window_end,COALESCE(w.requests,usage.completed)::bigint AS requests,
               usage.tokens,usage.cost_micro,usage.unknown_cost_requests FROM period_window CROSS JOIN usage
        LEFT JOIN channel_usage_windows w ON w.channel_id=$1 AND w.period=$2 AND w.window_start=period_window.window_start
    ").bind(channel).bind(period).bind(zone).bind(interval).fetch_one(&mut **tx).await?)
}

async fn read_requests_only(
    tx: &mut Transaction<'_, Postgres>,
    channel: i64,
    period: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>, i64), StoreError> {
    let interval = match period {
        "hour" => "1 hour",
        "day" => "1 day",
        "week" => "1 week",
        "month" => "1 month",
        _ => return Err(StoreError::InvalidData("usage_period")),
    };
    let zone = timezone::reporting_timezone()?;
    // PostgreSQL short-circuits COALESCE: once channel_usage_windows has a row for
    // this window, the bucket fallback sum is never executed.
    Ok(sqlx::query_as(
        r"
        WITH bounds AS (SELECT date_trunc($2,now() AT TIME ZONE $3) AS start,
                               date_trunc($2,now(),$3) AS window_start),
        period_window AS (SELECT window_start,
                  CASE WHEN $2='hour' THEN window_start + interval '1 hour'
                       ELSE (start + $4::text::interval) AT TIME ZONE $3 END AS window_end FROM bounds)
        SELECT period_window.window_start,
               period_window.window_end,
               COALESCE(
                   w.requests,
                   (SELECT COALESCE(SUM(requests),0)::bigint
                      FROM channel_usage_buckets
                     WHERE channel_id=$1
                       AND bucket_start >= period_window.window_start
                       AND bucket_start < period_window.window_end)
               )::bigint AS requests
          FROM period_window
          LEFT JOIN channel_usage_windows w
            ON w.channel_id=$1 AND w.period=$2 AND w.window_start=period_window.window_start
        ",
    )
    .bind(channel)
    .bind(period)
    .bind(zone)
    .bind(interval)
    .fetch_one(&mut **tx)
    .await?)
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
    let (window_start, window_end, current_requests) = if tokens.is_none() && cost.is_none() {
        let (window_start, window_end, current_requests) =
            read_requests_only(&mut tx, channel, period).await?;
        if requests.is_some_and(|cap| current_requests >= cap) {
            tx.commit().await?;
            return Ok(false);
        }
        (window_start, window_end, current_requests)
    } else {
        let usage = read(&mut tx, channel, period).await?;
        if requests.is_some_and(|cap| usage.requests >= cap)
            || tokens.is_some_and(|cap| usage.tokens >= cap)
            || cost.is_some_and(|cap| usage.unknown_cost_requests > 0 || usage.cost_micro >= cap)
        {
            tx.commit().await?;
            return Ok(false);
        }
        (usage.window_start, usage.window_end, usage.requests)
    };
    sqlx::query(
        r"INSERT INTO channel_usage_windows(channel_id,period,window_start,window_end,requests)
                  VALUES($1,$2,$3,$4,$5) ON CONFLICT(channel_id,period,window_start)
                  DO UPDATE SET requests=channel_usage_windows.requests+1",
    )
    .bind(channel)
    .bind(period)
    .bind(window_start)
    .bind(window_end)
    .bind(current_requests.saturating_add(1))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn prune(pool: &PgPool) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM channel_usage_windows WHERE ctid IN (SELECT ctid FROM channel_usage_windows WHERE window_end < now() - interval '45 days' LIMIT 1000)")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM channel_usage_buckets WHERE ctid IN (SELECT ctid FROM channel_usage_buckets WHERE bucket_start < now() - interval '45 days' LIMIT 1000)")
        .execute(pool)
        .await?;
    Ok(())
}
