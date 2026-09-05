//! 展示真实消费水位与积压，查询时刻不充当数据更新时间。
use crate::gateway::{error::AppError, state::AppState};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub async fn read(state: &AppState) -> Result<Value, AppError> {
    let Some(ch) = &state.ch else {
        return Ok(Value::Null);
    };
    let rows = ch.query_json_each_row("SELECT toUnixTimestamp64Milli(maxMerge(last_event)) AS event_ms, toUnixTimestamp64Milli(maxMerge(last_ingested)) AS ingest_ms FROM mv_analysis_hour").await?;
    let event_ms = rows
        .first()
        .map_or(0, |r| super::stats::ch_i64(r, "event_ms"));
    let ingest_ms = rows
        .first()
        .map_or(0, |r| super::stats::ch_i64(r, "ingest_ms"));
    let (pending, oldest, failed): (i64, Option<DateTime<Utc>>, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 0), min(created_at) FILTER (WHERE status = 0), count(*) FILTER (WHERE status = 2) FROM billing_outbox WHERE status IN (0, 2)"
    ).fetch_one(&state.pg).await.map_err(okapi_store::StoreError::from)?;
    let latest: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT created_at FROM billing_outbox ORDER BY id DESC LIMIT 1")
            .fetch_optional(&state.pg)
            .await
            .map_err(okapi_store::StoreError::from)?;
    let now = Utc::now();
    let age = oldest.map(|ts| (now - ts).num_seconds().max(0));
    let gap = latest.map(|ts| {
        if event_ms > 0 {
            ((ts.timestamp_millis() - event_ms) / 1000).max(0)
        } else {
            (now - ts).num_seconds().max(0)
        }
    });
    Ok(json!({
        "last_event_at": (event_ms > 0).then(|| DateTime::<Utc>::from_timestamp_millis(event_ms)).flatten(),
        "last_ingested_at": (ingest_ms > 0).then(|| DateTime::<Utc>::from_timestamp_millis(ingest_ms)).flatten(),
        "pending_events": pending, "failed_events": failed,
        "oldest_pending_at": oldest, "queue_age_seconds": age, "event_gap_seconds": gap,
        "stale": is_stale(age, gap, failed),
        "checked_at": now
    }))
}

fn is_stale(queue_age: Option<i64>, event_gap: Option<i64>, failed: i64) -> bool {
    queue_age.is_some_and(|n| n >= 60) || event_gap.is_some_and(|n| n >= 60) || failed > 0
}
#[cfg(test)]
mod tests {
    #[test]
    fn idle_is_not_stale_but_backlog_or_failed_delivery_is() {
        assert!(!super::is_stale(None, None, 0));
        assert!(!super::is_stale(None, Some(0), 0));
        assert!(!super::is_stale(Some(30), Some(30), 0));
        assert!(super::is_stale(Some(60), None, 0));
        assert!(super::is_stale(None, Some(60), 0));
        assert!(super::is_stale(None, None, 1));
    }
}
