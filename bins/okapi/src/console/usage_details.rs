//! 日历窗口与门户附加指标；历史缺失的采集维度返回 null，不显示成零。
use super::stats::ch_i64;
use crate::gateway::error::AppError;
use chrono::{Days, NaiveDate};
use okapi_store::ChClient;
use serde_json::{Value, json};
use std::collections::HashMap;

pub struct CalendarWindow {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub today: String,
    pub timezone: String,
    pub generated_at: String,
}

impl CalendarWindow {
    pub async fn read(
        ch: &ChClient,
        days: u32,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Self, AppError> {
        let rows = ch.query_json_each_row("SELECT toString(today()) AS today, timezone() AS timezone, toString(now()) AS generated_at").await?;
        let meta = rows.first().ok_or_else(AppError::internal)?;
        let today = meta["today"].as_str().ok_or_else(AppError::internal)?;
        let current = parse_date(today)?;
        let (start, end) = Self::bounds(current, days, start, end)?;
        Ok(Self {
            start,
            end,
            today: today.to_owned(),
            timezone: meta["timezone"].as_str().unwrap_or("UTC").to_owned(),
            generated_at: meta["generated_at"].as_str().unwrap_or_default().to_owned(),
        })
    }

    fn bounds(
        today: NaiveDate,
        days: u32,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<(NaiveDate, NaiveDate), AppError> {
        match (start, end) {
            (None, None) => Ok((today - Days::new(u64::from(days.clamp(1, 366) - 1)), today)),
            (Some(start), Some(end)) => {
                let start = parse_date(start)?;
                let end = parse_date(end)?;
                if start > end || end > today || (end - start).num_days() >= 366 {
                    return Err(AppError::bad_request().with_param("date_range"));
                }
                Ok((start, end))
            }
            _ => Err(AppError::bad_request().with_param("date_range")),
        }
    }

    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }
    pub fn day_filter(&self) -> String {
        format!(
            "day >= toDate('{}') AND day <= toDate('{}')",
            self.start, self.end
        )
    }
    pub fn json(&self) -> Value {
        json!({"start_date": self.start.to_string(), "end_date": self.end.to_string(), "today": self.today,
            "timezone": self.timezone, "generated_at": self.generated_at})
    }
}

fn parse_date(value: &str) -> Result<NaiveDate, AppError> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|date| date.to_string() == value && value >= "1970-01-01" && value <= "2148-12-31")
        .ok_or_else(|| AppError::bad_request().with_param("date_range"))
}

fn row_key(row: &Value) -> (String, String) {
    (
        row["day"].as_str().unwrap_or_default().to_owned(),
        row["model"].as_str().unwrap_or_default().to_owned(),
    )
}

/// owner/range 只来自认证整数与校验后的日期，不接受用户 SQL。
pub async fn enrich(
    ch: &ChClient,
    owner: &str,
    range: &str,
    data: &mut [Value],
) -> Result<Value, AppError> {
    let cache_sql = format!(
        "SELECT day, model, sumMerge(write_tokens) AS writes, countIfMerge(known_requests) AS known \
        FROM mv_cache_write_day WHERE {owner} AND {range} GROUP BY day, model"
    );
    let perf_sql = format!(
        "SELECT toDate(hour) AS day, model, countMerge(requests) AS samples, \
        sumMerge(latency_sum) AS latency, sumMerge(ttft_sum) AS ttft, countIfMerge(ttft_samples) AS ttft_n, \
        sumMerge(completion_tokens) AS output FROM mv_cube_hour WHERE {owner} AND {range} GROUP BY day, model"
    );
    let cache = ch.query_json_each_row(&cache_sql).await?;
    let perf = ch.query_json_each_row(&perf_sql).await?;
    let cache: HashMap<_, _> = cache.iter().map(|r| (row_key(r), r)).collect();
    let perf: HashMap<_, _> = perf.iter().map(|r| (row_key(r), r)).collect();
    let mut counts = [0_i64; 8]; // requests, known cache, writes, perf samples, latency, ttft, ttft samples, output
    for row in data {
        let key = row_key(row);
        let cache = cache.get(&key);
        let perf = perf.get(&key);
        let requests = ch_i64(row, "requests");
        let known = cache.map_or(0, |r| ch_i64(r, "known"));
        let writes = cache.map_or(0, |r| ch_i64(r, "writes"));
        let samples = perf.map_or(0, |r| ch_i64(r, "samples"));
        let latency = perf.map_or(0, |r| ch_i64(r, "latency"));
        let ttft = perf.map_or(0, |r| ch_i64(r, "ttft"));
        let ttft_n = perf.map_or(0, |r| ch_i64(r, "ttft_n"));
        let output = perf.map_or(0, |r| ch_i64(r, "output"));
        row["cache_write_tokens"] = if known == requests {
            json!(writes)
        } else {
            Value::Null
        };
        row["cache_write_known_requests"] = json!(known);
        row["avg_latency_ms"] = nullable_ratio(latency, samples, requests, samples, 1);
        row["avg_ttft_ms"] = nullable_ratio(ttft, ttft_n, requests, samples, 1);
        row["tokens_per_1k_sec"] = nullable_ratio(output, latency, requests, samples, 1_000_000);
        row["performance_requests"] = json!(samples);
        row["latency_sum_ms"] = json!(latency);
        row["ttft_sum_ms"] = json!(ttft);
        row["ttft_samples"] = json!(ttft_n);
        row["original_micro"] =
            json!(ch_i64(row, "amount_micro").saturating_add(ch_i64(row, "discount_micro")));
        for (acc, value) in counts.iter_mut().zip([
            requests, known, writes, samples, latency, ttft, ttft_n, output,
        ]) {
            *acc = acc.saturating_add(value);
        }
    }
    Ok(json!({
        "cache_write_tokens": if counts[0] == counts[1] { json!(counts[2]) } else { Value::Null },
        "cache_write_known_requests": counts[1],
        "avg_latency_ms": nullable_ratio(counts[4], counts[3], counts[0], counts[3], 1),
        "avg_ttft_ms": nullable_ratio(counts[5], counts[6], counts[0], counts[3], 1),
        "tokens_per_1k_sec": nullable_ratio(counts[7], counts[4], counts[0], counts[3], 1_000_000),
        "performance_requests": counts[3]
    }))
}

fn nullable_ratio(sum: i64, divisor: i64, expected: i64, samples: i64, scale: i64) -> Value {
    if divisor > 0 && samples == expected {
        json!(sum.saturating_mul(scale) / divisor)
    } else {
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_window_is_inclusive_and_rejects_invalid_ranges() {
        let today = parse_date("2024-03-01").unwrap();
        let (start, end) = CalendarWindow::bounds(today, 7, None, None).unwrap();
        assert_eq!(start.to_string(), "2024-02-24");
        assert_eq!((end - start).num_days(), 6);
        assert!(CalendarWindow::bounds(today, 7, Some("2024-02-29"), Some("2024-03-01")).is_ok());
        for (start, end) in [
            ("2023-02-29", "2024-03-01"),
            ("2024-03-01", "2024-03-02"),
            ("2024-03-01", "2024-02-29"),
            ("2022-01-01", "2024-01-01"),
        ] {
            assert!(CalendarWindow::bounds(today, 7, Some(start), Some(end)).is_err());
        }
        assert_eq!(nullable_ratio(0, 0, 0, 0, 1), Value::Null);
        assert_eq!(nullable_ratio(1200, 2, 4, 2, 1), Value::Null);
        assert_eq!(nullable_ratio(1200, 2, 2, 2, 1), json!(600));
    }
}
