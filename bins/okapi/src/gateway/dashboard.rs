//! new-api / OpenAI 生态兼容的余额查询端点（§11.1「key 余额公开查询」）：
//! 客户端（ChatGPT-Next-Web 等）用 subscription/usage 差值显示剩余额度。
//! 口径：hard_limit_usd = (余额 + 累计消费) 即总额度；usage 返回美分。

use super::error::AppError;
use super::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde_json::{Value, json};

async fn totals(state: &AppState, headers: &HeaderMap) -> Result<(i64, i64), AppError> {
    let key = super::auth::authenticate(state, headers).await?;
    let balance = state.ledger.balance(key.user_id).await?.as_micros();
    let used = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(used_micro), 0)::bigint AS "u!"
           FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL"#,
        key.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok((balance, used))
}

/// 展示层 USD 字符串（分粒度，非计费路径）。
fn micro_to_usd_json(micro: i64) -> Value {
    // 生态客户端期望 JSON number：以分精度构造（整数除法两段拼）
    let cents = micro / 10_000;
    let value = format!("{}.{:02}", cents / 100, (cents % 100).abs());
    value
        .parse::<serde_json::Number>()
        .map_or(json!(0), Value::Number)
}

/// GET /v1/dashboard/billing/subscription
pub async fn subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (balance, used) = totals(&state, &headers).await?;
    Ok(Json(json!({
        "object": "billing_subscription",
        "has_payment_method": true,
        "hard_limit_usd": micro_to_usd_json(balance.saturating_add(used)),
        "soft_limit_usd": micro_to_usd_json(balance.saturating_add(used)),
        "system_hard_limit_usd": micro_to_usd_json(balance.saturating_add(used)),
        "access_until": 0,
    })))
}

/// GET /v1/dashboard/billing/usage（生态口径：total_usage 为美分）
pub async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let (_, used) = totals(&state, &headers).await?;
    Ok(Json(json!({
        "object": "list",
        "total_usage": micro_to_usd_json(used.saturating_mul(100)),
    })))
}
