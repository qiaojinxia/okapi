//! 上游余额查询（IMPLEMENTATION §11.33，对照 new-api 渠道"更新余额"）。
//!
//! 探针是厂商数据（`okapi-compat` 的声明式 `ProbeSpec`，见 `docs/compat-rules.md` §17.6），这里只管
//! 执行：选探针 → 带渠道凭证依次 GET → 交给探针规格解析。选择顺序：渠道明确选了的厂商（档案取它
//! 叠加的内置厂商）→ 按 `api_base` 主机识别出的厂商 → 通用的 new-api / one-api / OpenAI dashboard
//! 口径（`/dashboard/billing/subscription` + `/usage`）。anthropic / gemini / azure / custom_pass
//! 没有公开余额接口，直接告知不支持。金额是**该货币**的 micro 整数：上游十进制经定点解析，不经浮点。

use super::admin::{ensure_channel_owner, guard_scoped};
use crate::gateway::error::AppError;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::permissions;
use okapi_compat::vendors::{self, BalanceChoice};
use serde_json::{Value, json};

/// 上游余额响应体上限：这些接口都是几百字节的 JSON，8KB 已经很宽。
const BALANCE_MAX_BYTES: usize = 8 * 1024;

/// 按协议、厂商与主机选探针；None = 该渠道没有可查的余额接口。
fn select_probe(provider: &str, vendor: Option<&str>, api_base: &str) -> Option<BalanceChoice> {
    if !matches!(provider, "openai" | "openai_compat") {
        return None;
    }
    let host = reqwest::Url::parse(api_base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .unwrap_or_default();
    Some(vendors::balance_probe_for(vendor, &host))
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
        SELECT c.provider, c.api_base, c.settings, ck.id AS key_id, ck.credential_ciphertext
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
    // 没配地址时只有官方 openai 有缺省；兼容渠道不能回退到别家的地址
    let base = okapi_providers::registry::effective_base(&row.provider, row.api_base.as_deref())
        .ok_or_else(|| AppError::bad_request().with_param("api_base"))?;
    let base = base.trim_end_matches('/');
    let vendor = okapi_store::compat::builtin_vendor_of_channel(&state.pg, channel_id).await?;
    let Some(choice) = select_probe(&row.provider, vendor.as_deref(), base) else {
        return Err(AppError::bad_request().with_param("balance_unsupported"));
    };
    let credential =
        okapi_store::credential::open(state.master_key.as_deref(), &row.credential_ciphertext)?;
    let outbound = okapi_providers::Outbound::from_settings(
        &row.settings,
        super::egress::key_proxy(&state, row.key_id).await?,
    );

    let today = chrono::Utc::now().date_naive();
    let date = |offset: i64| (today + chrono::Duration::days(offset)).to_string();
    let mut bodies = Vec::new();
    for url in choice.probe.urls(base, &date) {
        bodies.push(get_json(&state, url, &credential, &outbound).await?);
    }
    let balance = choice
        .probe
        .parse(&bodies)
        .ok_or_else(|| AppError::bad_request().with_param("balance_shape"))?;

    let result = json!({
        "channel_id": channel_id,
        "probe": choice.name,
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
        .probe(okapi_providers::custom_pass::PassRequest {
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
        Ok(okapi_providers::custom_pass::PassResponse::Ok { stream, .. }) => {
            super::outbound_body::collect(stream, BALANCE_MAX_BYTES).await?
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
    use okapi_compat::probe::DASHBOARD_NAME;

    fn chosen(provider: &str, vendor: Option<&str>, base: &str) -> Option<&'static str> {
        select_probe(provider, vendor, base).map(|choice| choice.name)
    }

    #[test]
    fn probe_selection_by_host_and_provider() {
        assert_eq!(
            chosen("openai_compat", None, "https://api.deepseek.com/v1"),
            Some("deepseek")
        );
        assert_eq!(
            chosen("openai_compat", None, "https://api.siliconflow.cn/v1"),
            Some("siliconflow")
        );
        assert_eq!(
            chosen("openai_compat", None, "https://openrouter.ai/api/v1"),
            Some("openrouter")
        );
        assert_eq!(
            chosen("openai", None, "https://api.moonshot.cn/v1"),
            Some("moonshot")
        );
        assert_eq!(
            chosen("openai_compat", None, "https://relay.example.com/v1"),
            Some(DASHBOARD_NAME)
        );
        assert_eq!(
            chosen("anthropic", None, "https://api.anthropic.com/v1"),
            None
        );
        assert_eq!(chosen("azure", None, "https://x.openai.azure.com"), None);
    }

    #[test]
    fn a_chosen_vendor_wins_over_the_host() {
        // 中转站换了域名、本机 mock、地址解析不了：选了厂商就照那家的口径查
        for base in [
            "https://relay.example.com/v1",
            "http://127.0.0.1:9/v1",
            "not a url",
        ] {
            assert_eq!(
                chosen("openai_compat", Some("builtin:deepseek"), base),
                Some("deepseek"),
                "{base}"
            );
        }
        // 协议不支持的仍然不支持，厂商救不了
        assert_eq!(
            chosen(
                "anthropic",
                Some("builtin:deepseek"),
                "https://api.deepseek.com/anthropic"
            ),
            None
        );
    }

    #[test]
    fn deepseek_url_leaves_version_segment() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let date = |offset: i64| (today + chrono::Duration::days(offset)).to_string();
        let urls = |choice: BalanceChoice, base: &str| choice.probe.urls(base, &date);
        let deepseek = select_probe("openai_compat", None, "https://api.deepseek.com/v1").unwrap();
        assert_eq!(
            urls(deepseek, "https://api.deepseek.com/v1"),
            vec!["https://api.deepseek.com/user/balance".to_owned()]
        );
        let generic = select_probe("openai_compat", None, "https://relay.example.com/v1").unwrap();
        let dash = urls(generic, "https://relay.example.com/v1");
        assert_eq!(
            dash[0],
            "https://relay.example.com/v1/dashboard/billing/subscription"
        );
        assert_eq!(
            dash[1],
            "https://relay.example.com/v1/dashboard/billing/usage?end_date=2026-09-07&start_date=2026-05-29"
        );
    }
}
