//! 日历窗口与门户附加指标；历史缺失的采集维度返回 null，不显示成零。
use super::stats::{ch_i64, scaled_ratio};
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

    pub(super) fn bounds(
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
        .filter(|date| date.to_string() == value && ("1970-01-01"..="2148-12-31").contains(&value))
        .ok_or_else(|| AppError::bad_request().with_param("date_range"))
}

fn row_key(row: &Value) -> (String, String) {
    (
        row["day"].as_str().unwrap_or_default().to_owned(),
        row["model"].as_str().unwrap_or_default().to_owned(),
    )
}

async fn source_rows(
    ch: &ChClient,
    owner: &str,
    range: &str,
) -> Result<HashMap<(String, String), Value>, AppError> {
    let source_sql = super::token_details::with_provenance(
        "day, model",
        "mv_key_model_day",
        &format!("{owner} AND {range}"),
    );
    let sources = ch
        .query_json_each_row(&format!("SELECT * FROM {source_sql}"))
        .await?;
    Ok(sources.into_iter().map(|r| (row_key(&r), r)).collect())
}

type ModelDayRows = HashMap<(String, String), Value>;

fn cache_write_metrics(writes: i64, requests: i64, known: i64) -> [(&'static str, Value); 3] {
    [
        (
            "cache_write_tokens",
            if known == requests {
                json!(writes)
            } else {
                Value::Null
            },
        ),
        (
            "recorded_cache_write_tokens",
            if known > 0 || writes > 0 {
                json!(writes)
            } else {
                Value::Null
            },
        ),
        ("cache_write_known_requests", json!(known)),
    ]
}

async fn performance_rows(
    ch: &ChClient,
    owner: &str,
    range: &str,
) -> Result<(ModelDayRows, ModelDayRows), AppError> {
    let predicate = format!("{owner} AND {range}");
    let latency = super::latency::source("day, model", "mv_cube_hour", &predicate);
    let rate = super::output_rate::prepared(
        "day, model",
        "mv_cube_hour",
        &predicate,
        super::measurement_coverage::Mode::Recover,
    );
    let latency = ch
        .query_json_each_row(&format!("SELECT * FROM {latency}"))
        .await?;
    let rate = ch
        .query_json_each_row(&format!("SELECT * FROM {rate}"))
        .await?;
    Ok((
        latency
            .into_iter()
            .map(|row| (row_key(&row), row))
            .collect(),
        rate.into_iter().map(|row| (row_key(&row), row)).collect(),
    ))
}

/// owner/range 只来自认证整数与校验后的日期，不接受用户 SQL。
pub async fn enrich(
    ch: &ChClient,
    owner: &str,
    range: &str,
    data: &mut [Value],
) -> Result<Value, AppError> {
    let sources = source_rows(ch, owner, range).await?;
    let mut source_totals = json!({});
    let (perf, rates) = performance_rows(ch, owner, range).await?;
    let ttft_sql = super::ttft_average::source(
        "day, model",
        "mv_cube_hour",
        &format!("{owner} AND {range}"),
    );
    let ttft = ch
        .query_json_each_row(&format!("SELECT * FROM {ttft_sql}"))
        .await?;
    let ttft: HashMap<_, _> = ttft.iter().map(|r| (row_key(r), r)).collect();
    let cache = super::cache_usage::query(
        ch,
        "day, model",
        "mv_key_model_day",
        &format!("{owner} AND {range}"),
    )
    .await?;
    let cache: HashMap<_, _> = cache.iter().map(|r| (row_key(r), r)).collect();
    let mut counts = [0_i64; 14]; // requests, known writes, writes, perf samples, latency, ttft, ttft samples, output, known reads, prompt, cached, ttft observed, latency observed
    for row in data {
        let key = row_key(row);
        let empty = json!({});
        let provenance = sources.get(&key).unwrap_or(&empty);
        super::input_units::correct_totals(row, provenance)?;
        let cache = cache.get(&key);
        let perf = perf.get(&key);
        let requests = ch_i64(row, "requests");
        let known = cache.map_or(0, |r| ch_i64(r, "cache_write_n"));
        let read_known = cache.map_or(0, |r| ch_i64(r, "cache_read_n"));
        let prompt = ch_i64(row, "prompt_tokens");
        let cached = ch_i64(row, "cached_tokens");
        let writes = cache.map_or(0, |r| ch_i64(r, "cache_writes"));
        let samples = perf.map_or(0, |r| ch_i64(r, "latency_samples"));
        let latency = perf.map_or(0, |r| ch_i64(r, "latency_sum"));
        let measured = ttft.get(&key);
        let ttft = measured.map_or(0, |r| ch_i64(r, "ttft_sum"));
        let ttft_n = measured.map_or(0, |r| ch_i64(r, "ttft_samples"));
        let observed = measured.map_or(0, |r| ch_i64(r, "ttft_observed"));
        let output = perf.map_or(0, |r| ch_i64(r, "latency_output"));
        for (name, value) in cache_write_metrics(writes, requests, known) {
            row[name] = value;
        }
        row["cache_read_known_requests"] = json!(read_known);
        let rate = rates.get(&key).unwrap_or(&empty);
        super::output_rate::accumulate(&mut source_totals, rate);
        super::usage_sources::accumulate(&mut source_totals, provenance);
        super::token_details::accumulate(&mut source_totals, provenance);
        for (name, value) in super::usage_sources::metrics(
            provenance,
            requests,
            [Some(prompt), Some(ch_i64(row, "completion_tokens"))],
            Some(cached),
            read_known,
        )
        .into_iter()
        .chain(super::token_details::metrics(provenance, requests))
        {
            row[name] = value;
        }
        let latency_observed = perf.map_or(0, |r| ch_i64(r, "latency_observed"));
        for (name, value) in
            super::latency::metrics(latency, samples, output, requests, latency_observed)
        {
            row[name] = value;
        }
        for (name, value) in super::output_rate::metrics(rate, requests) {
            row[name] = value;
        }
        for (name, value) in super::ttft_average::metrics(ttft, ttft_n, requests, observed) {
            row[name] = value;
        }
        row["original_micro"] =
            json!(ch_i64(row, "amount_micro").saturating_add(ch_i64(row, "discount_micro")));
        for (acc, value) in counts.iter_mut().zip([
            requests,
            known,
            writes,
            samples,
            latency,
            ttft,
            ttft_n,
            output,
            read_known,
            prompt,
            cached,
            observed,
            latency_observed,
            ch_i64(row, "completion_tokens"),
        ]) {
            *acc = acc.saturating_add(value);
        }
    }
    Ok(total_metrics(counts, &source_totals))
}

fn total_metrics(counts: [i64; 14], source_totals: &Value) -> Value {
    let mut total = json!({
        "cache_read_known_requests": counts[8],
        "cache_hit_bp": cache_rate(
            counts[10],
            counts[9],
            super::usage_sources::cache_eligible_requests(source_totals, counts[0], Some(counts[9])),
            counts[8],
        ),

    });
    for (name, value) in cache_write_metrics(counts[2], counts[0], counts[1]) {
        total[name] = value;
    }
    for (name, value) in super::ttft_average::metrics(counts[5], counts[6], counts[0], counts[11]) {
        total[name] = value;
    }
    for (name, value) in
        super::latency::metrics(counts[4], counts[3], counts[7], counts[0], counts[12])
    {
        total[name] = value;
    }
    for (name, value) in super::output_rate::metrics(source_totals, counts[0]) {
        total[name] = value;
    }
    for (name, value) in super::usage_sources::metrics(
        source_totals,
        counts[0],
        [Some(counts[9]), Some(counts[13])],
        Some(counts[10]),
        counts[8],
    )
    .into_iter()
    .chain(super::token_details::metrics(source_totals, counts[0]))
    {
        total[name] = value;
    }
    total
}

pub(super) fn cache_rate(cached: i64, prompt: i64, requests: i64, known: i64) -> Value {
    nullable_ratio(cached, prompt, requests, known, 10_000)
}

fn nullable_ratio(sum: i64, divisor: i64, expected: i64, samples: i64, scale: i64) -> Value {
    if divisor > 0 && samples == expected {
        json!(scaled_ratio(sum, divisor, scale))
    } else {
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rates_keep_precision_when_scaled_totals_exceed_i64() {
        assert_eq!(
            scaled_ratio(20_000_000_000_000, 200_000_000_000, 1_000_000),
            100_000_000
        );
        assert_eq!(
            cache_rate(2_000_000_000_000_000, 4_000_000_000_000_000, 10, 10),
            json!(5000)
        );
        assert_eq!(
            nullable_ratio(20_000_000_000_000, 1440, 10, 10, 1_000_000),
            json!(13_888_888_888_888_888_i64)
        );
        assert_eq!(
            scaled_ratio(-2_000_000_000_000_000, 4_000_000_000_000_000, 10_000),
            -5000
        );
        assert_eq!(scaled_ratio(i64::MAX, 1, 10_000), i64::MAX);
        assert_eq!(scaled_ratio(i64::MIN, 1, 10_000), i64::MIN);
        assert_eq!(scaled_ratio(1, 0, 1000), 0);
    }
    #[test]
    fn cache_rate_distinguishes_missing_zero_and_weighted_hits() {
        assert_eq!(cache_rate(0, 100, 1, 0), Value::Null);
        assert_eq!(cache_rate(0, 100, 1, 1), json!(0));
        assert_eq!(cache_rate(100, 1000, 2, 2), json!(1000));
        assert_eq!(cache_rate(100, 1000, 2, 1), Value::Null);
        assert_eq!(cache_rate(0, 0, 0, 0), Value::Null);
    }
    #[test]
    fn partial_cache_quantities_do_not_require_a_complete_hit_rate() {
        let mut counts = [0; 14];
        counts[0] = 8;
        counts[1] = 1;
        counts[2] = 20;
        counts[8] = 1;
        counts[9] = 673;
        counts[10] = 90;
        let total = total_metrics(counts, &json!({}));
        assert_eq!(total["cache_write_tokens"], Value::Null);
        assert_eq!(total["recorded_cache_write_tokens"], 20);
        assert_eq!(total["cache_hit_bp"], Value::Null);
        assert_eq!(total["cache_read_known_requests"], 1);
        counts[2] = 0;
        assert_eq!(
            total_metrics(counts, &json!({}))["recorded_cache_write_tokens"],
            0
        );
        counts[1] = 0;
        assert_eq!(
            total_metrics(counts, &json!({}))["recorded_cache_write_tokens"],
            Value::Null
        );
    }
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
