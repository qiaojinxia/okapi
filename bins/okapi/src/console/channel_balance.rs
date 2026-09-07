//! 上游余额查询（IMPLEMENTATION §11.33，对照 new-api 渠道"更新余额"）。
//!
//! 探针按 `api_base` 主机选择，不加配置项：几家按余额计费的官方上游各有私有接口，
//! 其余 openai / openai_compat 一律走 new-api / one-api 生态口径
//! （`/dashboard/billing/subscription` + `/usage`）。anthropic / gemini / azure / custom_pass
//! 没有公开余额接口，直接告知不支持。金额是**该货币**的 micro 整数：上游十进制经
//! `parse_scaled_1e6` 定点解析，不经浮点。

use super::admin::{ensure_channel_owner, guard_scoped};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::permissions;
use serde_json::{Value, json};

/// 上游余额响应体上限：这些接口都是几百字节的 JSON，8KB 已经很宽。
const BALANCE_MAX_BYTES: usize = 8 * 1024;

/// 余额探针类型。`name()` 回给前端与留痕，让管理员知道这个数是按哪家口径算的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    /// new-api / one-api / OpenAI 老 dashboard 口径：额度 − 用量。
    OpenAiDashboard,
    DeepSeek,
    SiliconFlow,
    OpenRouter,
    Moonshot,
}

impl Probe {
    /// 按协议与主机选探针；None = 该渠道没有可查的余额接口。
    pub(crate) fn for_channel(provider: &str, api_base: &str) -> Option<Self> {
        if !matches!(provider, "openai" | "openai_compat") {
            return None;
        }
        let host = reqwest::Url::parse(api_base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_lowercase))
            .unwrap_or_default();
        Some(match host.as_str() {
            "api.deepseek.com" => Self::DeepSeek,
            "api.siliconflow.cn" | "api.siliconflow.com" => Self::SiliconFlow,
            "openrouter.ai" => Self::OpenRouter,
            "api.moonshot.cn" | "api.moonshot.ai" => Self::Moonshot,
            _ => Self::OpenAiDashboard,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::OpenAiDashboard => "openai_dashboard",
            Self::DeepSeek => "deepseek",
            Self::SiliconFlow => "siliconflow",
            Self::OpenRouter => "openrouter",
            Self::Moonshot => "moonshot",
        }
    }

    /// 要请求的 URL 列表（dashboard 口径要两条）。`base` 已去尾斜杠且含版本段（`.../v1`）。
    fn urls(self, base: &str, today: chrono::NaiveDate) -> Vec<String> {
        match self {
            Self::OpenAiDashboard => {
                // new-api 自己实现的这两个端点忽略日期；OpenAI 老接口要求 end_date 开区间
                let start = today - chrono::Duration::days(100);
                let end = today + chrono::Duration::days(1);
                vec![
                    format!("{base}/dashboard/billing/subscription"),
                    format!("{base}/dashboard/billing/usage?start_date={start}&end_date={end}"),
                ]
            }
            // DeepSeek 的余额接口在站点根，不在 /v1 下
            Self::DeepSeek => vec![format!("{}/user/balance", strip_version(base))],
            Self::SiliconFlow => vec![format!("{base}/user/info")],
            Self::OpenRouter => vec![format!("{base}/credits")],
            Self::Moonshot => vec![format!("{base}/users/me/balance")],
        }
    }

    /// 把上游响应（与 `urls()` 同序）解析成统一形状；None = 形状不认。
    fn parse(self, bodies: &[Value]) -> Option<Balance> {
        match self {
            Self::OpenAiDashboard => parse_dashboard(bodies.first()?, bodies.get(1)?),
            Self::DeepSeek => parse_deepseek(bodies.first()?),
            Self::SiliconFlow => parse_siliconflow(bodies.first()?),
            Self::OpenRouter => parse_openrouter(bodies.first()?),
            Self::Moonshot => parse_moonshot(bodies.first()?),
        }
    }
}

/// 统一余额形状：`remaining_micro` 为该货币的 micro 整数（响应字段名 `balance_micro`）；
/// 总额 / 已用只有部分上游给。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Balance {
    pub currency: &'static str,
    pub remaining_micro: i64,
    pub total_micro: Option<i64>,
    pub used_micro: Option<i64>,
}

fn strip_version(base: &str) -> &str {
    base.strip_suffix("/v1").unwrap_or(base)
}

/// JSON 数字或十进制字符串 → micro 整数。负数保留符号；超过 6 位小数截断；
/// 科学计数法等认不出的形状返回 None。
pub(crate) fn decimal_micro(value: &Value) -> Option<i64> {
    let text = match value {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_owned(),
        _ => return None,
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.as_str()),
    };
    let digits = match digits.split_once('.') {
        Some((int, frac)) if frac.len() > 6 => format!("{int}.{}", &frac[..6]),
        _ => digits.to_owned(),
    };
    let scaled = okapi_pricing::ratio::parse_scaled_1e6(&digits).ok()?;
    Some(if negative { -scaled } else { scaled })
}

/// `subscription.hard_limit_usd`（美元）− `usage.total_usage`（美分）。
fn parse_dashboard(subscription: &Value, usage: &Value) -> Option<Balance> {
    let total = decimal_micro(subscription.get("hard_limit_usd")?)?;
    // total_usage 是美分：美分的 micro ÷ 100 = 美元的 micro
    let used = decimal_micro(usage.get("total_usage")?)? / 100;
    Some(Balance {
        currency: "USD",
        remaining_micro: total.checked_sub(used)?,
        total_micro: Some(total),
        used_micro: Some(used),
    })
}

/// `{"balance_infos":[{"currency":"CNY","total_balance":"110.00",...}]}`。
fn parse_deepseek(body: &Value) -> Option<Balance> {
    let info = body.get("balance_infos")?.as_array()?.first()?;
    let currency = match info.get("currency").and_then(Value::as_str) {
        Some("USD") => "USD",
        _ => "CNY",
    };
    Some(Balance {
        currency,
        remaining_micro: decimal_micro(info.get("total_balance")?)?,
        total_micro: None,
        used_micro: None,
    })
}

/// `{"data":{"totalBalance":"12.34",...}}`（人民币）。
fn parse_siliconflow(body: &Value) -> Option<Balance> {
    Some(Balance {
        currency: "CNY",
        remaining_micro: decimal_micro(body.get("data")?.get("totalBalance")?)?,
        total_micro: None,
        used_micro: None,
    })
}

/// `{"data":{"total_credits":10.0,"total_usage":3.5}}`（美元）。
fn parse_openrouter(body: &Value) -> Option<Balance> {
    let data = body.get("data")?;
    let total = decimal_micro(data.get("total_credits")?)?;
    let used = decimal_micro(data.get("total_usage")?)?;
    Some(Balance {
        currency: "USD",
        remaining_micro: total.checked_sub(used)?,
        total_micro: Some(total),
        used_micro: Some(used),
    })
}

/// `{"data":{"available_balance":49.58,...}}`（人民币）。
fn parse_moonshot(body: &Value) -> Option<Balance> {
    Some(Balance {
        currency: "CNY",
        remaining_micro: decimal_micro(body.get("data")?.get("available_balance")?)?,
        total_micro: None,
        used_micro: None,
    })
}

/// GET /admin/channels/{id}/balance：查上游余额并留痕（`ch:balance:<id>`，列表回填）。
/// 与测活 / 拉模型同门（`channel.write` + own 范围）；只读不审计。
pub async fn fetch_channel_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    let row = sqlx::query!(
        r#"
        SELECT c.provider, c.api_base, c.settings, ck.credential_ciphertext
        FROM channels c
        JOIN channel_keys ck ON ck.channel_id = c.id
        WHERE c.id = $1 AND c.deleted_at IS NULL
        ORDER BY ck.id
        LIMIT 1
        "#,
        channel_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            okapi_api::codes::NO_AVAILABLE_CHANNEL,
        )
    })?;
    let base = row.api_base.unwrap_or_default();
    let base = base.trim_end_matches('/');
    let Some(probe) = Probe::for_channel(&row.provider, base) else {
        return Err(AppError::bad_request().with_param("balance_unsupported"));
    };
    let credential =
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
    let outbound = okapi_providers::Outbound::from_settings(&row.settings);

    let mut bodies = Vec::new();
    for url in probe.urls(base, chrono::Utc::now().date_naive()) {
        bodies.push(get_json(&state, url, &credential, &outbound).await?);
    }
    let balance = probe
        .parse(&bodies)
        .ok_or_else(|| AppError::bad_request().with_param("balance_shape"))?;

    let result = json!({
        "channel_id": channel_id,
        "probe": probe.name(),
        "currency": balance.currency,
        "balance_micro": balance.remaining_micro,
        "total_micro": balance.total_micro,
        "used_micro": balance.used_micro,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    state
        .sched
        .channel_balance_record(channel_id, &result)
        .await;
    Ok(Json(result))
}

/// 带渠道凭证 GET 一个小 JSON（走渠道自己的代理 / 额外头）。
async fn get_json(
    state: &AppState,
    url: String,
    credential: &str,
    outbound: &okapi_providers::Outbound,
) -> Result<Value, AppError> {
    let outcome = state
        .pass
        .forward(okapi_providers::custom_pass::PassRequest {
            method: axum::http::Method::GET,
            url,
            auth_header: "authorization".to_owned(),
            auth_value: format!("Bearer {credential}"),
            content_type: None,
            body: bytes::Bytes::new(),
            proxy_url: outbound.proxy_url.clone(),
            extra_headers: outbound.extra_headers.clone(),
        })
        .await;
    let body = match outcome {
        Ok(okapi_providers::custom_pass::PassResponse::Ok { mut stream, .. }) => {
            use futures::StreamExt as _;
            let mut buf: Vec<u8> = Vec::new();
            while let Some(Ok(chunk)) = stream.next().await {
                if buf.len() + chunk.len() > BALANCE_MAX_BYTES {
                    return Err(AppError::bad_request().with_param("balance_shape"));
                }
                buf.extend_from_slice(&chunk);
            }
            buf
        }
        Ok(okapi_providers::custom_pass::PassResponse::ErrStatus { status, .. }) => {
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                    .with_param(format!("status_{status}")),
            );
        }
        Err(err) => {
            return Err(
                AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                    .with_param(err.error_code()),
            );
        }
    };
    serde_json::from_slice(&body).map_err(|_| AppError::bad_request().with_param("balance_shape"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_selection_by_host_and_provider() {
        assert_eq!(
            Probe::for_channel("openai_compat", "https://api.deepseek.com/v1"),
            Some(Probe::DeepSeek)
        );
        assert_eq!(
            Probe::for_channel("openai_compat", "https://api.siliconflow.cn/v1"),
            Some(Probe::SiliconFlow)
        );
        assert_eq!(
            Probe::for_channel("openai_compat", "https://openrouter.ai/api/v1"),
            Some(Probe::OpenRouter)
        );
        assert_eq!(
            Probe::for_channel("openai", "https://api.moonshot.cn/v1"),
            Some(Probe::Moonshot)
        );
        assert_eq!(
            Probe::for_channel("openai_compat", "https://relay.example.com/v1"),
            Some(Probe::OpenAiDashboard)
        );
        assert_eq!(
            Probe::for_channel("anthropic", "https://api.anthropic.com/v1"),
            None
        );
        assert_eq!(
            Probe::for_channel("azure", "https://x.openai.azure.com"),
            None
        );
    }

    #[test]
    fn deepseek_url_leaves_version_segment() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        assert_eq!(
            Probe::DeepSeek.urls("https://api.deepseek.com/v1", today),
            vec!["https://api.deepseek.com/user/balance".to_owned()]
        );
        let dash = Probe::OpenAiDashboard.urls("https://relay.example.com/v1", today);
        assert_eq!(
            dash[0],
            "https://relay.example.com/v1/dashboard/billing/subscription"
        );
        assert_eq!(
            dash[1],
            "https://relay.example.com/v1/dashboard/billing/usage?start_date=2026-05-29&end_date=2026-09-07"
        );
    }

    #[test]
    fn decimal_parsing_is_exact_and_tolerant() {
        assert_eq!(decimal_micro(&json!("110.00")), Some(110_000_000));
        assert_eq!(decimal_micro(&json!(49.58)), Some(49_580_000));
        assert_eq!(decimal_micro(&json!(100)), Some(100_000_000));
        assert_eq!(decimal_micro(&json!("-3.5")), Some(-3_500_000));
        // 7 位小数截断而非拒绝
        assert_eq!(decimal_micro(&json!("1.23456789")), Some(1_234_567));
        assert_eq!(decimal_micro(&json!(true)), None);
        assert_eq!(decimal_micro(&json!("abc")), None);
    }

    #[test]
    fn dashboard_balance_is_limit_minus_usage_cents() {
        let sub = json!({"hard_limit_usd": 100.0, "has_payment_method": true});
        let usage = json!({"total_usage": 1234.5});
        let b = Probe::OpenAiDashboard.parse(&[sub, usage]).unwrap();
        assert_eq!(b.currency, "USD");
        assert_eq!(b.total_micro, Some(100_000_000));
        assert_eq!(b.used_micro, Some(12_345_000));
        assert_eq!(b.remaining_micro, 87_655_000);
    }

    #[test]
    fn vendor_shapes_parse() {
        let ds = json!({"is_available": true, "balance_infos": [
            {"currency": "CNY", "total_balance": "110.00", "granted_balance": "0.00"}]});
        let b = Probe::DeepSeek.parse(&[ds]).unwrap();
        assert_eq!((b.currency, b.remaining_micro), ("CNY", 110_000_000));

        let sf = json!({"code": 20000, "data": {"totalBalance": "12.34"}});
        let b = Probe::SiliconFlow.parse(&[sf]).unwrap();
        assert_eq!((b.currency, b.remaining_micro), ("CNY", 12_340_000));

        let or = json!({"data": {"total_credits": 10.0, "total_usage": 3.5}});
        let b = Probe::OpenRouter.parse(&[or]).unwrap();
        assert_eq!((b.currency, b.remaining_micro), ("USD", 6_500_000));

        let ms = json!({"code": 0, "data": {"available_balance": 49.58, "voucher_balance": 0}});
        let b = Probe::Moonshot.parse(&[ms]).unwrap();
        assert_eq!((b.currency, b.remaining_micro), ("CNY", 49_580_000));

        assert!(Probe::DeepSeek.parse(&[json!({"unexpected": 1})]).is_none());
    }
}
