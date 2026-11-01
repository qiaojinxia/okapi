//! 站点设置读写与 SMTP 测试。
use super::{audit, guard};
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::permissions;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct SetSettingReq {
    pub key: String,
    pub value: Value,
}

/// 全局设置写入（如 site_notice / model_rpm_limits）。
pub async fn set_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<SetSettingReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    // 全局默认出口有引用校验与固定分配对账，只能经 PUT /admin/egress/default 写
    if req.key == okapi_store::egress::DEFAULT_SETTING {
        return Err(AppError::bad_request().with_param("egress_default"));
    }
    let is_pricing_base = req.key == crate::gateway::pricing_loader::BASE_PRICE_SETTING;
    if req.key == "streaming_policy"
        && !crate::gateway::stream_policy::StreamPolicy::valid(&req.value)
    {
        return Err(AppError::bad_request().with_param("streaming_policy"));
    }
    if req.key == "oauth_refresh_policy"
        && crate::worker::oauth_refresh::RefreshPolicy::parse(&req.value).is_none()
    {
        return Err(AppError::bad_request().with_param("oauth_refresh_policy"));
    }
    if req.key == "egress_probe_policy" {
        let Some(policy) = crate::worker::egress_probe::ProbePolicy::parse(&req.value) else {
            return Err(AppError::bad_request().with_param("egress_probe_policy"));
        };
        // 探测地址由后台按时请求：与上游地址同一道 SSRF 闸
        if let Some(target) = policy.target.as_deref() {
            crate::gateway::ssrf::validate_api_base(&state.pg, target).await?;
        }
    }
    // 清理任务按 `(value #>> '{}')::bigint` 读：null / 小数 / 字符串会让它每轮报错且永不清理
    if req.key == "retention_months" && req.value.as_u64().is_none_or(|months| months > 1200) {
        return Err(AppError::bad_request().with_param("retention_months"));
    }
    if is_pricing_base && crate::gateway::pricing_loader::valid_base_price(&req.value).is_none() {
        return Err(AppError::bad_request().with_param("pricing_base_per_1m_micro"));
    }
    okapi_store::admin::set_setting(&state.pg, &req.key, &req.value, actor.actor_user_id()).await?;
    state.invalidate_routing_caches();
    // settings 热路径缓存按键失效：同进程立即生效（公告发布、限流阈值调整不必等 60s TTL），
    // 多副本靠 TTL 收敛——与鉴权缓存"本机即时、跨副本 TTL"同一取舍
    state.settings_cache.invalidate(&req.key).await;
    audit(
        &state,
        &actor,
        "settings.set",
        &req.key,
        if super::super::manage::is_secret_key(&req.key) {
            json!({ "redacted": true, "configured": !req.value.is_null() })
        } else {
            json!({ "value": req.value })
        },
    )
    .await;
    Ok(Json(
        json!({ "ok": true, "requires_publish": is_pricing_base }),
    ))
}

/// GET /admin/settings/{key}：单键读取（配置页回显；写权限同门槛防低权限窥探敏感配置）。
pub async fn get_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = $1"#, key)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({ "key": key, "value": value })))
}

#[derive(Deserialize)]
pub struct SmtpTestReq {
    pub to: String,
    #[serde(default)]
    pub lang: Option<String>,
}

/// POST /admin/settings/smtp/test：用**已保存**的 settings.smtp 发一封测试信（§11.27）。
/// 未配置 501 `smtp_not_configured`；发送失败 502 `smtp_send_failed` + 参数带上游错误摘要
/// （SMTP 错误码是排障唯一线索，此处不算"自然语言文案"）。
pub async fn smtp_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<SmtpTestReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    let to = req.to.trim();
    if !to.contains('@') {
        return Err(AppError::bad_request().with_param("to"));
    }
    // 绕过 60s 设置缓存：配置页"保存→测试"是连贯操作，必须读到刚保存的值
    let mailer = crate::mail::Mailer::from_pg(&state.pg)
        .await
        .map_err(|error| match error {
            crate::mail::MailError::NotConfigured => {
                AppError::new(StatusCode::NOT_IMPLEMENTED, "smtp_not_configured")
            }
            other => {
                tracing::error!(error = %other, "SMTP 配置读取失败");
                AppError::internal()
            }
        })?;
    let accept = headers
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    let lang = crate::mail::templates::Lang::resolve(req.lang.as_deref(), accept);
    let site = state
        .setting_cached("site_name")
        .await
        .as_ref()
        .as_ref()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "Okapi".to_owned());
    if let Err(err) = mailer
        .send(crate::mail::templates::test_message(lang, &site, to))
        .await
    {
        tracing::warn!(error = %err, "SMTP 测试发送失败");
        return Err(
            AppError::new(StatusCode::BAD_GATEWAY, "smtp_send_failed").with_param(err.to_string())
        );
    }
    audit(&state, &actor, "settings.smtp_test", to, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}
