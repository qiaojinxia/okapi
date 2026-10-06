//! User-owned billing logs and full-filter aggregates share one SQL predicate.
//! Money comes from PostgreSQL, never from the currently loaded page or CH.

use super::{LogWindow, LogsQuery};
use crate::console::query::Query;
use crate::gateway::{auth::authenticate, error::AppError, state::AppState};
use axum::{Json, extract::State, http::HeaderMap};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::HashMap;

fn filtered(
    sql: &str,
    q: &LogsQuery,
    window: Option<&LogWindow>,
    user_id: i64,
    key_id: i64,
) -> Result<QueryBuilder<Postgres>, AppError> {
    if q.api_key_id.is_some_and(|id| id <= 0) {
        return Err(AppError::bad_request().with_param("api_key_id"));
    }
    let mut query = QueryBuilder::new(sql);
    query.push(" FROM billing_records b LEFT JOIN api_keys k ON k.id = b.api_key_id AND k.user_id = b.user_id WHERE b.user_id = ").push_bind(user_id);
    if q.scope.as_deref() != Some("user") {
        query.push(" AND b.api_key_id = ").push_bind(key_id);
    }
    if let Some(id) = q.api_key_id {
        query.push(" AND b.api_key_id = ").push_bind(id);
    }
    if let Some(id) = q.request_id {
        query.push(" AND b.request_id = ").push_bind(id);
    }
    if let Some(model) = q.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        query.push(" AND b.model_name = ").push_bind(model);
    }
    if q.errors_only == Some(true) {
        // Request failure and financial disposition are independent: an all-failed
        // batch releases its hold (status=30), while log_type=5 retains its outcome.
        query.push(" AND (b.log_type = 5 OR b.status = 40 OR b.usage_details->'diagnostics'->>'request_failed' = 'true')");
    }
    if let Some(window) = window {
        query
            .push(" AND b.created_at >= (")
            .push_bind(window.start)
            .push("::date::timestamp AT TIME ZONE ")
            .push_bind(&window.timezone)
            .push(")");
        query
            .push(" AND b.created_at < ((")
            .push_bind(window.end)
            .push("::date + 1)::timestamp AT TIME ZONE ")
            .push_bind(&window.timezone)
            .push(")");
    }
    Ok(query)
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let window = q.window(&state.pg).await?;
    let mut query = filtered(
        r"SELECT jsonb_build_object(
            'id', b.id, 'request_id', b.request_id, 'model', b.model_name,
            'log_type', b.log_type, 'status', b.status,
            'is_error', (b.log_type = 5 OR b.status = 40 OR COALESCE(b.usage_details->'diagnostics'->>'request_failed' = 'true', false)),
            'upstream_request_id', b.upstream_request_id,
            'api_key_id', b.api_key_id, 'key_name', COALESCE(k.name, ''),
            'usage', jsonb_build_object(
                'prompt_tokens', b.prompt_tokens, 'cached_tokens', b.cached_tokens,
                'input_unit', b.usage_details->'input_unit', 'input_characters', b.usage_details->'input_characters',
                'completion_tokens', b.completion_tokens, 'reasoning_tokens', b.reasoning_tokens,
                'upstream_usage', b.usage_details->'tokens'->'upstream_usage',
                'reported_details', b.usage_details->'tokens'->'reported_details',
                'server_tool_usage', b.usage_details->'tokens'->'server_tool_usage',
                'prompt_source', COALESCE(b.usage_details->>'prompt_source', 'unknown'),
                'completion_source', COALESCE(b.usage_details->>'completion_source', 'unknown'),
                'cache_read_reported', COALESCE((b.usage_details->'tokens'->>'cache_read_reported')::boolean, NULLIF(b.cached_tokens > 0, false)),
                'cache_write_reported', (b.usage_details->'tokens'->>'cache_write_reported')::boolean,
                'cache_write_tokens', b.usage_details->'tokens'->'cache_write_tokens',
                'cache_write_5m_tokens', b.usage_details->'tokens'->'cache_write_5m_tokens',
                'cache_write_1h_tokens', b.usage_details->'tokens'->'cache_write_1h_tokens',
                'audio_prompt_tokens', b.usage_details->'tokens'->'audio_prompt_tokens',
                'image_prompt_tokens', b.usage_details->'tokens'->'image_prompt_tokens',
                'audio_completion_tokens', b.usage_details->'tokens'->'audio_completion_tokens',
                'image_completion_tokens', b.usage_details->'tokens'->'image_completion_tokens',
                'cache_read_modalities', b.usage_details->'tokens'->'cache_read_modalities',
                'cache_write_modalities', b.usage_details->'tokens'->'cache_write_modalities'
            ),
            'usage_details_recorded', b.usage_details IS NOT NULL,
            'requested_model', NULLIF(b.usage_details->>'requested_model', ''),
            'endpoint', NULLIF(b.usage_details->>'endpoint', ''),
            'pool', b.pool, 'amount_micro', b.amount_micro,
            'net_amount_micro', CASE WHEN b.status = 20 THEN b.amount_micro ELSE 0 END,
            'original_amount_micro', b.original_amount_micro, 'discount_micro', b.discount_micro,
            'pricing_snapshot', b.pricing_snapshot, 'error_code', b.error_code,
            'diagnostics', b.usage_details->'diagnostics',
            'latency_ms', b.latency_ms, 'ttft_ms', b.ttft_ms,
            'is_stream', b.is_stream, 'created_at', b.created_at
        )",
        &q,
        window.as_ref(),
        key.user_id,
        key.key_id,
    )?;
    if let Some(before) = q.before {
        query.push(" AND b.id < ").push_bind(before);
    }
    let limit = q.limit.clamp(1, 200);
    // Fetch one extra row so exactly-full final pages do not advertise another page.
    query
        .push(" ORDER BY b.id DESC LIMIT ")
        .push_bind(limit + 1);
    let mut data: Vec<Value> = query
        .build_query_scalar()
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let has_more = data.len() > usize::try_from(limit).unwrap_or(200);
    if has_more {
        data.pop();
    }
    for row in &mut data {
        if let Some(diagnostics) = row.get_mut("diagnostics").filter(|d| !d.is_null()) {
            *diagnostics = crate::gateway::diagnostics::public(diagnostics);
        }
    }
    let next_before = if has_more {
        data.last().and_then(|r| r["id"].as_i64())
    } else {
        None
    };
    Ok(Json(json!({
        "scope": if q.scope.as_deref() == Some("user") { "user" } else { "key" },
        "window": window.map(|w| json!({ "start_date": w.start, "end_date": w.end, "timezone": w.timezone })),
        "data": data, "next_before": next_before,
    })))
}

pub async fn stat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let window = q.window(&state.pg).await?;
    // Deliberately ignore cursor/limit: the summary describes ALL matching ledger rows.
    // Refunds change the original row to 30, retaining its original amount and usage.
    let sources = crate::console::usage_sources::pg_sql();
    let observations = crate::console::usage_observations::pg_sql();
    let units = crate::console::input_units::pg_sql();
    let output_rate = crate::console::output_rate::pg_sql();
    let summary = format!(
        r"SELECT jsonb_build_object(
            'records', COUNT(*),
            'settled', COUNT(*) FILTER (WHERE b.status = 20),
            'failed', COUNT(*) FILTER (WHERE b.status = 40),
            'errors', COUNT(*) FILTER (WHERE b.log_type = 5 OR b.status = 40 OR b.usage_details->'diagnostics'->>'request_failed' = 'true'),
            'refunded', COUNT(*) FILTER (WHERE b.status = 30),
            'pending', COUNT(*) FILTER (WHERE b.status = 10),
            'amount_micro', COALESCE(SUM(b.amount_micro) FILTER (WHERE b.status = 20), 0),
            'refunded_amount_micro', COALESCE(SUM(b.amount_micro) FILTER (WHERE b.status = 30), 0),
            'prompt_tokens', COALESCE(SUM(b.prompt_tokens::bigint), 0),
            'completion_tokens', COALESCE(SUM(b.completion_tokens::bigint), 0),
            'cached_tokens', COALESCE(SUM(b.cached_tokens::bigint), 0),
            'cache_read_samples', COUNT(*) FILTER (WHERE COALESCE((b.usage_details->'tokens'->>'cache_read_reported')::boolean, b.cached_tokens > 0)),
            'cache_write_tokens', SUM((b.usage_details->'tokens'->>'cache_write_tokens')::bigint) FILTER (WHERE COALESCE((b.usage_details->'tokens'->>'cache_write_reported')::boolean, false) OR (b.usage_details->'tokens'->>'cache_write_tokens')::bigint > 0),
            'cache_write_samples', COUNT(*) FILTER (WHERE COALESCE((b.usage_details->'tokens'->>'cache_write_reported')::boolean, false) OR (b.usage_details->'tokens'->>'cache_write_tokens')::bigint > 0),
            'cache_write_5m_tokens', SUM((b.usage_details->'tokens'->>'cache_write_5m_tokens')::bigint),
            'cache_write_1h_tokens', SUM((b.usage_details->'tokens'->>'cache_write_1h_tokens')::bigint),
            'cache_write_ttl_samples', COUNT(*) FILTER (WHERE b.usage_details->'tokens'->>'cache_write_5m_tokens' IS NOT NULL AND b.usage_details->'tokens'->>'cache_write_1h_tokens' IS NOT NULL),
            'reasoning_tokens', COALESCE(SUM(b.reasoning_tokens::bigint), 0),
            'audio_prompt_tokens', SUM((b.usage_details->'tokens'->>'audio_prompt_tokens')::bigint),
            'image_prompt_tokens', SUM((b.usage_details->'tokens'->>'image_prompt_tokens')::bigint),
            'audio_completion_tokens', SUM((b.usage_details->'tokens'->>'audio_completion_tokens')::bigint),
            'image_completion_tokens', SUM((b.usage_details->'tokens'->>'image_completion_tokens')::bigint),
            'audio_prompt_samples', COUNT(b.usage_details->'tokens'->>'audio_prompt_tokens'),
            'image_prompt_samples', COUNT(b.usage_details->'tokens'->>'image_prompt_tokens'),
            'audio_completion_samples', COUNT(b.usage_details->'tokens'->>'audio_completion_tokens'),
            'image_completion_samples', COUNT(b.usage_details->'tokens'->>'image_completion_tokens'),
            'cache_read_audio_tokens', SUM((b.usage_details->'tokens'->'cache_read_modalities'->>'audio_tokens')::bigint),
            'cache_read_image_tokens', SUM((b.usage_details->'tokens'->'cache_read_modalities'->>'image_tokens')::bigint),
            'cache_write_audio_tokens', SUM((b.usage_details->'tokens'->'cache_write_modalities'->>'audio_tokens')::bigint),
            'cache_write_image_tokens', SUM((b.usage_details->'tokens'->'cache_write_modalities'->>'image_tokens')::bigint),
            'cache_read_modal_samples', COUNT(b.usage_details->'tokens'->'cache_read_modalities'->>'audio_tokens'),
            'cache_write_modal_samples', COUNT(b.usage_details->'tokens'->'cache_write_modalities'->>'audio_tokens'),
            'avg_latency_ms', FLOOR(AVG(b.latency_ms) FILTER (WHERE b.status IN (20,30,40) AND b.latency_ms >= 0)),
            'latency_samples', COUNT(*) FILTER (WHERE b.status IN (20,30,40) AND b.latency_ms >= 0),
            'avg_ttft_ms', FLOOR(AVG(b.ttft_ms) FILTER (WHERE b.status IN (20,30,40) AND b.is_stream AND b.ttft_ms >= 0)),
            'ttft_samples', COUNT(*) FILTER (WHERE b.status IN (20,30,40) AND b.is_stream AND b.ttft_ms >= 0)
        ) || jsonb_build_object({sources}) || jsonb_build_object({observations}) || jsonb_build_object({units}) || jsonb_build_object({output_rate})"
    );
    let mut query = filtered(&summary, &q, window.as_ref(), key.user_id, key.key_id)?;
    let mut result: Value = query
        .build_query_scalar()
        .fetch_one(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let observed_row = result.clone();
    crate::console::usage_observations::enrich(
        &observed_row,
        &mut result,
        crate::console::stats::ch_i64(&observed_row, "records"),
    );
    if let Some(object) = result.as_object_mut() {
        object.extend(crate::console::input_units::metrics(
            &observed_row,
            crate::console::stats::ch_i64(&observed_row, "records"),
        ));
    }
    let metrics = crate::console::usage_sources::metrics(
        &result,
        crate::console::stats::ch_i64(&result, "records"),
        [
            Some(crate::console::stats::ch_i64(&result, "prompt_tokens")),
            Some(crate::console::stats::ch_i64(&result, "completion_tokens")),
        ],
        Some(crate::console::stats::ch_i64(&result, "cached_tokens")),
        crate::console::stats::ch_i64(&result, "cache_read_samples"),
    );
    if let Some(object) = result.as_object_mut() {
        for field in crate::console::usage_sources::FIELDS {
            object.remove(field);
        }
        object.retain(|name, _| !name.starts_with("observed_"));
        object.extend(metrics);
        for field in crate::console::output_rate::FIELDS {
            object.remove(field);
        }
        object.extend(crate::console::output_rate::metrics(
            &observed_row,
            crate::console::stats::ch_i64(&observed_row, "records"),
        ));
    }
    Ok(Json(result))
}

/// `GET /api/me/logs/series` 的参数：与 `stat` 同一套归属过滤（`scope` / `api_key_id`），外加回看窗口。
#[derive(serde::Deserialize)]
pub struct SeriesQuery {
    /// 回看天数（含今天），1–90，缺省 7。
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub scope: Option<String>,
    pub api_key_id: Option<i64>,
    /// IANA 时区；缺省 UTC，与列表里"近 7 天 Token"迷你折线同一口径。
    #[serde(default)]
    pub timezone: Option<String>,
}

/// 按日用量序列（密钥用量折线图）。
///
/// 数据源与 `list` / `stat` 完全相同——PG 账本 + 同一条 `filtered` 谓词，金额不经 CH、不受
/// 当前页影响；窗口内每个自然日都有一行（没有调用的日子补零，由服务端补，前端不必猜时区）。
/// 口径：`requests` 是账本行数，`tokens` 是输入 + 输出，`amount_micro` 只计已结算（status=20，
/// 退款行不计，同 `stat.amount_micro`），`errors` 与 `stat.errors` 同一判定。
pub async fn series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SeriesQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let timezone = q.timezone.as_deref().unwrap_or("UTC");
    if timezone.len() > 128 {
        return Err(AppError::bad_request().with_param("timezone"));
    }
    // PG 校验 IANA 时区并给出当地"今天"：不用浏览器时钟，也不依赖 ClickHouse。
    let today = sqlx::query_scalar::<_, chrono::NaiveDate>(
        "SELECT (CURRENT_TIMESTAMP AT TIME ZONE name)::date FROM pg_timezone_names WHERE name = $1",
    )
    .bind(timezone)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| AppError::bad_request().with_param("timezone"))?;
    let days = q.days.unwrap_or(7).clamp(1, 90);
    let (start, end) =
        crate::console::usage_details::CalendarWindow::bounds(today, days, None, None)?;
    let window = LogWindow {
        start,
        end,
        timezone: timezone.to_owned(),
    };
    let filter = LogsQuery {
        limit: 0,
        before: None,
        scope: q.scope.clone(),
        model: None,
        errors_only: None,
        api_key_id: q.api_key_id,
        request_id: None,
        start_date: None,
        end_date: None,
        timezone: None,
    };
    // 时区名已在 pg_timezone_names 里逐字匹配过（只含字母数字与 `_+-/`），内联进分组表达式是安全的；
    // 仍转义单引号作兜底。绑定参数在 SELECT 前缀里不可用（`filtered` 只接受静态前缀）。
    let tz = timezone.replace('\'', "''");
    let day = format!("((b.created_at AT TIME ZONE '{tz}')::date)");
    let select = format!(
        r"SELECT jsonb_build_object(
            'day', {day}::text,
            'requests', COUNT(*),
            'errors', COUNT(*) FILTER (WHERE b.log_type = 5 OR b.status = 40 OR b.usage_details->'diagnostics'->>'request_failed' = 'true'),
            'tokens', COALESCE(SUM(b.prompt_tokens::bigint + b.completion_tokens::bigint), 0),
            'amount_micro', COALESCE(SUM(b.amount_micro) FILTER (WHERE b.status = 20), 0)
        )"
    );
    let mut query = filtered(&select, &filter, Some(&window), key.user_id, key.key_id)?;
    query.push(format!(" GROUP BY {day}"));
    let rows: Vec<Value> = query
        .build_query_scalar()
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let by_day: HashMap<String, &Value> = rows
        .iter()
        .filter_map(|row| Some((row["day"].as_str()?.to_owned(), row)))
        .collect();
    let mut total = [0_i64; 4];
    let data: Vec<Value> = start
        .iter_days()
        .take_while(|date| *date <= end)
        .map(|date| {
            let name = date.to_string();
            let cell = |field: &str| {
                by_day
                    .get(&name)
                    .map_or(0, |row| crate::console::stats::ch_i64(row, field))
            };
            let values = [
                cell("requests"),
                cell("errors"),
                cell("tokens"),
                cell("amount_micro"),
            ];
            for (sum, value) in total.iter_mut().zip(values) {
                *sum = sum.saturating_add(value);
            }
            json!({
                "day": name, "requests": values[0], "errors": values[1],
                "tokens": values[2], "amount_micro": values[3],
            })
        })
        .collect();
    Ok(Json(json!({
        "scope": if q.scope.as_deref() == Some("user") { "user" } else { "key" },
        "api_key_id": q.api_key_id,
        "days": days,
        "window": { "start_date": start.to_string(), "end_date": end.to_string(), "timezone": timezone },
        "total": { "requests": total[0], "errors": total[1], "tokens": total[2], "amount_micro": total[3] },
        "data": data,
    })))
}
