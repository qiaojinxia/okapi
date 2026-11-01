//! 只读工具：余额 / 用量 / key / 价目、账单解释、平台 KPI、渠道健康、日志检索、对账与死信。

use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_store::auth::{AuthedKey, PermScope};
use serde_json::{Value, json};

// ---- 用户级工具 ----

pub(super) async fn query_balance(state: &AppState, key: &AuthedKey) -> Result<Value, AppError> {
    let balance = state.ledger.balance(key.user_id).await?;
    Ok(json!({
        "user_id": key.user_id,
        "group": key.group_code,
        "balance_micro": balance.as_micros(),
    }))
}

pub(super) async fn query_usage(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_IMPLEMENTED, "stats_disabled"))?;
    let days = args
        .get("days")
        .and_then(Value::as_u64)
        .unwrap_or(7)
        .clamp(1, 90);
    let scope = args.get("scope").and_then(Value::as_str).unwrap_or("key");
    let sql = if scope == "user" {
        format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
             sumMerge(amount) AS amount_micro FROM mv_user_day \
             WHERE user_id = {} AND day >= today() - {days} GROUP BY day ORDER BY day",
            key.user_id
        )
    } else {
        format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
             sumMerge(amount) AS amount_micro FROM mv_apikey_day \
             WHERE api_key_id = {} AND day >= today() - {days} GROUP BY day ORDER BY day",
            key.key_id
        )
    };
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    Ok(json!({"scope": scope, "days": days, "data": rows}))
}

pub(super) async fn list_my_keys(state: &AppState, key: &AuthedKey) -> Result<Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT id, name, key_prefix, status, used_micro FROM api_keys
           WHERE user_id = $1 AND deleted_at IS NULL ORDER BY id"#,
        key.user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({"id": r.id, "name": r.name, "key_prefix": r.key_prefix,
                   "status": r.status, "used_micro": r.used_micro})
        })
        .collect();
    Ok(json!({"data": data}))
}

pub(super) async fn list_models_pricing(state: &AppState) -> Result<Value, AppError> {
    let rows = sqlx::query!(
        r#"SELECT m.model_name, p.pricing_mode,
                  p.model_ratio::text AS model_ratio,
                  p.completion_ratio::text AS completion_ratio,
                  p.cache_ratio::text AS cache_ratio,
                  p.per_call_price_micro
           FROM models m JOIN model_pricing p ON p.model_id = m.id
           WHERE m.status = 1 ORDER BY m.model_name"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({"model": r.model_name, "mode": r.pricing_mode,
                   "model_ratio": r.model_ratio, "completion_ratio": r.completion_ratio,
                   "cache_ratio": r.cache_ratio, "per_call_price_micro": r.per_call_price_micro})
        })
        .collect();
    Ok(json!({"data": data}))
}

/// 账单解释：非管理员只能看自己的记录（own 语义）。
pub(super) async fn explain_bill(
    state: &AppState,
    key: &AuthedKey,
    args: &Value,
) -> Result<Value, AppError> {
    let request_id = args
        .get("request_id")
        .and_then(Value::as_str)
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::bad_request().with_param("request_id"))?;
    let row = sqlx::query!(
        r#"SELECT request_id, user_id, model_name, status, log_type,
                  prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
                  amount_micro, original_amount_micro, discount_micro,
                  pricing_epoch, pricing_snapshot, error_code, created_at
           FROM billing_records WHERE request_id = $1"#,
        request_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_FOUND, "record_not_found"))?;
    let is_admin = key.permission_scope(permissions::BILLING_READ) == PermScope::All;
    if row.user_id != key.user_id && !is_admin {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "permission_denied",
        ));
    }
    Ok(json!({
        "request_id": row.request_id,
        "model": row.model_name,
        "status": row.status,
        "log_type": row.log_type,
        "usage": {
            "prompt_tokens": row.prompt_tokens,
            "cached_tokens": row.cached_tokens,
            "completion_tokens": row.completion_tokens,
            "reasoning_tokens": row.reasoning_tokens,
        },
        "amount_micro": row.amount_micro,
        "original_amount_micro": row.original_amount_micro,
        "discount_micro": row.discount_micro,
        "pricing_epoch": row.pricing_epoch,
        // 计费唯一语义载体（DESIGN §3）：修饰器逐层展开由 AI 按此解释
        "pricing_snapshot": row.pricing_snapshot,
        "error_code": row.error_code,
        "created_at": row.created_at.to_rfc3339(),
    }))
}

// ---- 管理查询工具 ----

pub(super) async fn platform_kpi(state: &AppState) -> Result<Value, AppError> {
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_IMPLEMENTED, "stats_disabled"))?;
    let rows = ch
        .query_json_each_row(
            "SELECT countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
             sumMerge(amount) AS amount_micro, uniqExact(user_id) AS active_users \
             FROM mv_user_day WHERE day = today()",
        )
        .await
        .map_err(AppError::from)?;
    Ok(json!({"today": rows.first().cloned().unwrap_or_else(|| json!({}))}))
}

pub(super) async fn channel_health(state: &AppState) -> Result<Value, AppError> {
    let channels = okapi_store::admin::list_channels(
        &state.pg,
        okapi_store::admin::ChannelFilter::default(),
        okapi_store::listing::Slice::ALL,
    )
    .await?;
    let keys = okapi_store::admin::list_channel_keys(&state.pg).await?;
    let data: Vec<Value> = channels
        .page
        .data
        .into_iter()
        .map(|c| {
            let keys: Vec<Value> = keys
                .iter()
                .filter(|k| k.channel_id == c.id)
                .map(
                    |k| json!({"id": k.id, "status": k.status, "cooldown_until": k.cooldown_until}),
                )
                .collect();
            json!({"id": c.id, "name": c.name, "provider": c.provider,
                   "status": c.status, "keys": keys})
        })
        .collect();
    Ok(json!({"data": data}))
}

/// 参数化维度模板（不暴露裸 SQL，继承 CH 查询护栏）。
pub(super) async fn usage_stats(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let ch = state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(axum::http::StatusCode::NOT_IMPLEMENTED, "stats_disabled"))?;
    let days = args
        .get("days")
        .and_then(Value::as_u64)
        .unwrap_or(7)
        .clamp(1, 90);
    let (mv, dim) = match args.get("dimension").and_then(Value::as_str) {
        Some("user") => ("mv_user_day", "user_id"),
        Some("model") => ("mv_user_model_day", "model"),
        Some("channel") => ("mv_channel_5min", "channel_id"),
        // 视图里的列叫 group_code；`group` 既不是列名、还是 SQL 保留字，这一维此前每次都查询报错
        Some("group") => ("mv_group_day", "group_code"),
        _ => return Err(AppError::bad_request().with_param("dimension")),
    };
    let time_col = if mv == "mv_channel_5min" {
        format!("ts5 >= now() - INTERVAL {days} DAY")
    } else {
        format!("day >= today() - {days}")
    };
    let sql = format!(
        "SELECT {dim} AS dim, countMerge(requests) AS requests, \
         sumMerge(amount) AS amount_micro FROM {mv} WHERE {time_col} \
         GROUP BY dim ORDER BY amount_micro DESC LIMIT 20"
    );
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    Ok(json!({"dimension": dim, "days": days, "data": rows}))
}

/// search_logs 的一行：request_id, user_id, model, status, amount, error_code, created_at。
pub(super) type LogRow = (
    uuid::Uuid,
    i64,
    String,
    i16,
    i64,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
);

pub(super) async fn search_logs(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 100);
    let user_id = args.get("user_id").and_then(Value::as_i64);
    let model = args.get("model").and_then(Value::as_str);
    let status = args
        .get("status")
        .and_then(Value::as_i64)
        .and_then(|v| i16::try_from(v).ok());
    let days = args
        .get("days")
        .and_then(Value::as_i64)
        .unwrap_or(7)
        .clamp(1, 90);
    // 只拼实际给出的过滤条件，并且总带时间下界：`$1 IS NULL OR user_id = $1` 这种写法在
    // 第 6 次执行后会换成通用计划、整个弃用索引；没有时间下界则要逐个分区扫全表。
    let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT request_id, user_id, model_name, status, amount_micro, error_code, created_at \
         FROM billing_records WHERE created_at >= now() - make_interval(days => ",
    );
    query.push_bind(i32::try_from(days).unwrap_or(7)).push(")");
    if let Some(user_id) = user_id {
        query.push(" AND user_id = ").push_bind(user_id);
    }
    if let Some(model) = model {
        query.push(" AND model_name = ").push_bind(model);
    }
    if let Some(status) = status {
        query.push(" AND status = ").push_bind(status);
    }
    query
        .push(" ORDER BY created_at DESC LIMIT ")
        .push_bind(limit);
    let rows: Vec<LogRow> = query
        .build_query_as()
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(
            |(request_id, user_id, model, status, amount, error_code, created_at)| {
                json!({"request_id": request_id, "user_id": user_id, "model": model,
                   "status": status, "amount_micro": amount,
                   "error_code": error_code, "created_at": created_at.to_rfc3339()})
            },
        )
        .collect();
    Ok(json!({"data": data}))
}

pub(super) async fn reconciliation_status(
    state: &AppState,
    args: &Value,
) -> Result<Value, AppError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(1000)
        .clamp(1, 5000);
    let drifts = okapi_kernel::reconcile::reconcile_balances(&state.pg, &state.ledger, limit)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "MCP 对账查询失败");
            AppError::internal()
        })?;
    let data: Vec<Value> = drifts
        .iter()
        .map(|d| {
            json!({"user_id": d.user_id, "events_sum_micro": d.events_sum_micro,
                   "redis_effective_micro": d.redis_effective_micro,
                   "pg_snapshot_micro": d.pg_snapshot_micro})
        })
        .collect();
    Ok(json!({"drift_count": data.len(), "drifts": data}))
}

pub(super) async fn dlq_list(state: &AppState, args: &Value) -> Result<Value, AppError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 200);
    let rows = sqlx::query!(
        r#"SELECT id, source, error, retry_count, created_at FROM billing_dlq
           WHERE status = 0 ORDER BY id DESC LIMIT $1"#,
        limit
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({"id": r.id, "source": r.source, "error": r.error,
                   "retry_count": r.retry_count, "created_at": r.created_at.to_rfc3339()})
        })
        .collect();
    Ok(json!({"data": data}))
}
