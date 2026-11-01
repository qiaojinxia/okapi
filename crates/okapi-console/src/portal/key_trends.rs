//! Seven UTC calendar days of ledger Token usage for the visible, user-owned keys.
//! One bounded batch query, independent of ClickHouse and log-page pagination.

use chrono::{Duration, NaiveDate};
use serde::Serialize;
use sqlx::PgPool;
use std::collections::HashMap;

#[derive(Serialize)]
pub(super) struct KeyTokenTrend {
    days: Vec<String>,
    tokens: [i64; 7],
    timezone: &'static str,
}

pub(super) async fn load(
    pg: &PgPool,
    user_id: i64,
    key_ids: &[i64],
    today: NaiveDate,
) -> Result<HashMap<i64, KeyTokenTrend>, sqlx::Error> {
    if key_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let start = today - Duration::days(6);
    let end = today + Duration::days(1);
    let rows: Vec<(i64, NaiveDate, i64)> = sqlx::query_as(
        "SELECT api_key_id, (created_at AT TIME ZONE 'UTC')::date AS day, \
                SUM(prompt_tokens::bigint + completion_tokens::bigint)::bigint AS tokens \
         FROM billing_records \
         WHERE user_id = $1 AND api_key_id = ANY($2) \
           AND created_at >= ($3::date::timestamp AT TIME ZONE 'UTC') \
           AND created_at < ($4::date::timestamp AT TIME ZONE 'UTC') \
         GROUP BY api_key_id, day",
    )
    .bind(user_id)
    .bind(key_ids)
    .bind(start)
    .bind(end)
    .fetch_all(pg)
    .await?;

    let days: Vec<String> = (0..7)
        .map(|day| (start + Duration::days(day)).to_string())
        .collect();
    // Zero-fill only after a successful query; a failed read must remain unavailable.
    let mut trends: HashMap<i64, KeyTokenTrend> = key_ids
        .iter()
        .map(|id| {
            (
                *id,
                KeyTokenTrend {
                    days: days.clone(),
                    tokens: [0; 7],
                    timezone: "UTC",
                },
            )
        })
        .collect();
    for (id, day, tokens) in rows {
        if let Some(trend) = trends.get_mut(&id)
            && let Ok(index) = usize::try_from((day - start).num_days())
            && let Some(value) = trend.tokens.get_mut(index)
        {
            *value = tokens;
        }
    }
    Ok(trends)
}
