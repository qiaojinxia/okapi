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
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    let wallet = state.ledger.balance(key.user_id).await?.as_micros();
    // 订阅池剩余计入"可用"（§11.28）：客户端 总额 − 已用 = 剩余 的口径仍对；
    // 只在池当前可用（now < sub_until）时计入，越界的负值钳 0
    let (sub, sub_until) = state.ledger.sub_balance(key.user_id).await?;
    let sub_avail = if sub_until > chrono::Utc::now().timestamp() {
        sub.as_micros().max(0)
    } else {
        0
    };
    let balance = wallet.saturating_add(sub_avail);
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
    // 生态客户端期望 JSON number：以分精度构造（整数除法两段拼）。
    // 符号单独处理：负余额在 −$1 以内时 cents / 100 是 0，不带符号会把 −0.15 显示成 0.15
    let cents = micro / 10_000;
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    let value = format!("{sign}{}.{:02}", cents / 100, cents % 100);
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

#[cfg(test)]
mod tests {
    use super::micro_to_usd_json;

    #[test]
    fn usd_amounts_keep_their_sign_and_cent_precision() {
        // serde_json 开了 arbitrary_precision：数字按文本比较，两位小数是对外格式的一部分
        let usd = |micro: i64| micro_to_usd_json(micro).to_string();
        assert_eq!(usd(1_230_000), "1.23");
        assert_eq!(usd(0), "0.00");
        // 负余额不到 $1：整数除法的商是 0，符号不能跟着丢
        assert_eq!(usd(-150_000), "-0.15");
        assert_eq!(usd(-10_000), "-0.01");
        assert_eq!(usd(-1_500_000), "-1.50");
        assert_eq!(usd(-12_345_600), "-12.34");
        // 不足一分的负数按分取整后是 0，不显示成 -0.00
        assert_eq!(usd(-5_000), "0.00");
        assert!(micro_to_usd_json(i64::MIN).is_number());
    }
}
