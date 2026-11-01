//! 计费端点共用的准入片段：计价上下文、预扣限额、预扣结果到 HTTP 错误的映射。
//!
//! chat / embeddings / audio / images / videos / realtime / custom_pass 七个入口原本各写一遍，
//! 漏改一处只有那条路径看得见（例：时区换算、限额字段）。准入顺序（成员月限、分组限流、
//! 渠道选择、失败退款）仍由各入口自己编排，这里只收它们逐字相同的那几步。

use super::rule_inputs::RuleInputs;
use super::state::AppState;
use axum::http::StatusCode;
use okapi_api::codes;
use okapi_domain::{GroupCode, ModelCode, Money, UserId};
use okapi_kernel::error::AppError;
use okapi_ledger::{LimitCaps, Pool, ReserveOutcome, ReserveRequest};
use okapi_pricing::{CalcContext, RatioFp};
use okapi_store::AuthedKey;
use uuid::Uuid;

/// 本次请求的计价上下文。`model` 是计价名（chat 的变体可能与路由名不同），
/// `service_tier` 只有 chat 会带（预扣按请求声明档估）。
pub(crate) fn calc_context(
    key: &AuthedKey,
    model: &str,
    rules_in: RuleInputs,
    now: chrono::DateTime<chrono::Utc>,
    service_tier: Option<String>,
) -> CalcContext {
    let utc_offset_seconds = now.with_timezone(&chrono::Local).offset().local_minus_utc();
    CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(model),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: rules_in.monthly_tokens,
        monthly_spend_micro: rules_in.monthly_spend_micro,
        local_minute_of_day: local_minute_of_day(now, utc_offset_seconds),
        now_unix: now.timestamp(),
        utc_offset_seconds,
        surge_active: rules_in.surge_active,
        service_tier,
    }
}

/// 站点本地时区的当日分钟数 0..1440（time_based 规则输入）。
fn local_minute_of_day(now: chrono::DateTime<chrono::Utc>, utc_offset_seconds: i32) -> u16 {
    u16::try_from(
        (now.timestamp()
            .saturating_add(i64::from(utc_offset_seconds))
            .div_euclid(60))
        .rem_euclid(1440),
    )
    .unwrap_or(0)
}

/// key 的 RPM / TPM / RPD / 并发上限（未配 = 0 = 不限）。
pub(crate) fn limit_caps(key: &AuthedKey) -> LimitCaps {
    let cap = |v: Option<i32>| v.map_or(0, i64::from);
    LimitCaps {
        rpm: cap(key.rpm_limit),
        tpm: cap(key.tpm_limit),
        rpd: cap(key.rpd_limit),
        concurrency: cap(key.max_concurrency),
    }
}

/// 预扣成功后的落点：所在池与订阅窗口（结算回到同一池）。
pub(crate) struct Reserved {
    pub pool: Pool,
    pub source_window: Option<String>,
}

/// 按 key 的限额预扣 `est`；余额不足与限流转成 429（`insufficient_quota` / `rate_limited`）。
pub(crate) async fn reserve(
    state: &AppState,
    key: &AuthedKey,
    request_id: Uuid,
    est: Money,
    est_tokens: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Reserved, AppError> {
    let outcome = state
        .reserve_for_key(
            key.quota_limited,
            ReserveRequest {
                user_id: key.user_id,
                api_key_id: key.key_id,
                request_id,
                est,
                caps: limit_caps(key),
                est_tokens,
            },
            now,
        )
        .await?;
    match outcome {
        ReserveOutcome::Reserved {
            pool,
            source_window,
            ..
        } => Ok(Reserved {
            pool,
            source_window,
        }),
        ReserveOutcome::Insufficient { .. } => Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            codes::INSUFFICIENT_QUOTA,
        )),
        ReserveOutcome::RateLimited { which } => {
            Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(which))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn minute_of_day_wraps_with_the_offset() {
        let at = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 23, 30, 0).unwrap();
        assert_eq!(local_minute_of_day(at, 0), 23 * 60 + 30);
        // UTC+8：次日 07:30
        assert_eq!(local_minute_of_day(at, 8 * 3600), 7 * 60 + 30);
        // UTC-5：当日 18:30
        assert_eq!(local_minute_of_day(at, -5 * 3600), 18 * 60 + 30);
    }
}
