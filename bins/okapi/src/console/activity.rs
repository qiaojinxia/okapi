//! 个人中心年度活动：只读长期日聚合，不受 raw 日志 180 天 TTL 影响。
use crate::gateway::{error::AppError, state::AppState};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use okapi_api::codes;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Key,
    User,
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    pub year: Option<u16>,
    pub scope: Option<Scope>,
}

fn owner_filter(user_id: i64, key_id: i64, user_scope: bool) -> String {
    if user_scope {
        format!("user_id = {user_id}")
    } else {
        format!("user_id = {user_id} AND api_key_id = {key_id}")
    }
}

fn checked_year(requested: Option<u16>, current: u16) -> Result<u16, AppError> {
    let year = requested.unwrap_or(current);
    if !(1970..=current.min(2148)).contains(&year) {
        return Err(AppError::bad_request().with_param("year"));
    }
    Ok(year)
}

fn year_sql(owner: &str, year: u16) -> String {
    format!(
        "SELECT day, model, countMerge(requests) AS requests, \
         sumMerge(prompt_tokens) AS prompt_tokens, sumMerge(cached_tokens) AS cached_tokens, \
         sumMerge(completion_tokens) AS completion_tokens, sumMerge(reasoning_tokens) AS reasoning_tokens, \
         sumMerge(amount) AS amount_micro, sumMerge(discount) AS discount_micro, sumMerge(errors) AS errors \
         FROM mv_key_model_day WHERE {owner} \
         AND day >= toDate('{year}-01-01') AND day < toDate('{}-01-01') AND day <= today() \
         GROUP BY day, model ORDER BY day, model",
        year + 1
    )
}

fn normalize_row(row: &Value) -> Value {
    let mut value = json!({ "day": row["day"], "model": row["model"] });
    for field in [
        "requests",
        "prompt_tokens",
        "cached_tokens",
        "completion_tokens",
        "reasoning_tokens",
        "amount_micro",
        "discount_micro",
        "errors",
    ] {
        value[field] = json!(super::stats::ch_i64(row, field));
    }
    value
}

/// GET /api/me/stats/activity?year=2026&scope=key|user
/// 沿用门户默认 Key 口径；只从已认证身份取 user_id/key_id。
pub async fn my_activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ActivityQuery>,
) -> Result<Json<Value>, AppError> {
    let key = crate::gateway::auth::authenticate(&state, &headers).await?;
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))?;
    let user_scope = matches!(q.scope, Some(Scope::User));
    let owner = owner_filter(key.user_id, key.key_id, user_scope);
    // day 来自 CH 服务端时区，返回同源 today/timezone，避免浏览器跨日偏移。
    let metadata = ch.query_json_each_row(&format!(
        "SELECT toString(today()) AS today, timezone() AS timezone, \
         toString(minOrNull(day)) AS first_day FROM mv_key_model_day WHERE {owner} AND day <= today()"
    )).await?;
    let meta = metadata.first().ok_or_else(AppError::internal)?;
    let today = meta["today"].as_str().ok_or_else(AppError::internal)?;
    let current = today
        .get(..4)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(AppError::internal)?;
    let year = checked_year(q.year, current)?;
    let first_year = meta["first_day"]
        .as_str()
        .and_then(|s| s.get(..4))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(current);
    let rows = ch.query_json_each_row(&year_sql(&owner, year)).await?;
    let mut data: Vec<Value> = rows.iter().map(normalize_row).collect();
    let range = format!(
        "day >= toDate('{year}-01-01') AND day < toDate('{}-01-01') AND day <= today()",
        year + 1
    );
    super::usage_details::enrich(ch, &owner, &range, &mut data).await?;
    Ok(Json(json!({
        "scope": if user_scope { "user" } else { "key" },
        "year": year, "today": today, "timezone": meta["timezone"],
        "first_year": first_year, "data": data
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_validates_calendar_years_and_scope() {
        assert_eq!(checked_year(None, 2026).unwrap(), 2026);
        assert_eq!(checked_year(Some(2024), 2026).unwrap(), 2024);
        assert!(checked_year(Some(2027), 2026).is_err());
        assert!(checked_year(Some(1969), 2026).is_err());
        assert!(serde_json::from_value::<ActivityQuery>(json!({"scope":"anything"})).is_err());
    }

    #[test]
    fn activity_queries_remain_owner_scoped_and_cover_entire_leap_year() {
        let key_sql = year_sql(&owner_filter(42, 7, false), 2024);
        assert!(key_sql.contains("user_id = 42 AND api_key_id = 7"));
        assert!(key_sql.contains("day >= toDate('2024-01-01') AND day < toDate('2025-01-01')"));
        assert!(key_sql.contains("day <= today()"));
        let user_sql = year_sql(&owner_filter(42, 7, true), 2024);
        assert!(user_sql.contains("WHERE user_id = 42 AND day"));
        assert!(!user_sql.contains("api_key_id"));
    }

    #[test]
    fn activity_normalizes_clickhouse_integer_strings() {
        let row = normalize_row(&json!({"day":"2024-02-29", "model":"m",
            "requests":"2", "prompt_tokens":"1000", "cached_tokens":200,
            "completion_tokens":"300", "reasoning_tokens":"100", "amount_micro":"1200"}));
        assert_eq!(row["prompt_tokens"], 1000);
        assert_eq!(row["completion_tokens"], 300);
        assert_eq!(row["cached_tokens"], 200);
        assert_eq!(row["amount_micro"], 1200);
        assert_eq!(row["errors"], 0);
    }
}
