//! 站点设置读写与 SMTP 测试。
use super::{audit, guard};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::permissions;
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_kernel::extract::Path;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct SetSettingReq {
    pub key: String,
    pub value: Value,
}

/// 全局设置写入（如 site_notices / model_rpm_limits）。
pub async fn set_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(mut req): ExtractJson<SetSettingReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    // 列表给的是打码后的值（支付 / OAuth 的密钥字段是占位串）：表单没改密钥就把占位串带回来，
    // 这里换回库里的原值；认领不到原值的占位串（新增的 OAuth 提供方）要求重新填写
    if !super::super::manage::is_secret_key(&req.key) {
        let stored = state.setting_fresh(&req.key).await?;
        req.value = super::super::manage::restore_redacted(req.value, stored.as_ref())
            .map_err(|()| AppError::bad_request().with_param(req.key.clone()))?;
    }
    // 全局默认出口有引用校验与固定分配对账，只能经 PUT /admin/egress/default 写
    if req.key == okapi_store::egress::DEFAULT_SETTING {
        return Err(AppError::bad_request().with_param("egress_default"));
    }
    let is_pricing_base = req.key == okapi_kernel::pricing_loader::BASE_PRICE_SETTING;
    if req.key == "streaming_policy"
        && !okapi_engine::stream_policy::StreamPolicy::valid(&req.value)
    {
        return Err(AppError::bad_request().with_param("streaming_policy"));
    }
    if req.key == "oauth_refresh_policy"
        && okapi_kernel::policies::RefreshPolicy::parse(&req.value).is_none()
    {
        return Err(AppError::bad_request().with_param("oauth_refresh_policy"));
    }
    if req.key == "egress_probe_policy" {
        let Some(policy) = okapi_kernel::policies::ProbePolicy::parse(&req.value) else {
            return Err(AppError::bad_request().with_param("egress_probe_policy"));
        };
        // 探测地址由后台按时请求：与上游地址同一道 SSRF 闸
        if let Some(target) = policy.target.as_deref() {
            okapi_kernel::ssrf::validate_api_base(&state.pg, target).await?;
        }
    }
    if req.key == okapi_kernel::policies::CHANNEL_PROBE_SETTING
        && okapi_kernel::policies::ChannelProbePolicy::parse(&req.value).is_none()
    {
        return Err(AppError::bad_request().with_param("channel_probe_policy"));
    }
    if req.key == okapi_kernel::policies::REQUEST_POLICY_SETTING
        && okapi_kernel::policies::RequestPolicy::parse(&req.value).is_none()
    {
        return Err(AppError::bad_request().with_param("request_policy"));
    }
    if req.key == okapi_kernel::policies::CHANNEL_BALANCE_SETTING
        && okapi_kernel::policies::ChannelBalancePolicy::parse(&req.value).is_none()
    {
        return Err(AppError::bad_request().with_param("channel_balance_policy"));
    }
    if req.key == okapi_kernel::policies::CONVERSATION_ARCHIVE_SETTING
        && okapi_kernel::policies::ConversationArchivePolicy::parse(&req.value).is_none()
    {
        return Err(AppError::bad_request().with_param("conversation_archive"));
    }
    // 公告列表整份校验：坏值不落库（公开接口读到坏条目只会悄悄跳过，站长以为发布了）
    if req.key == super::super::notices::SETTING && !super::super::notices::valid(&req.value) {
        return Err(AppError::bad_request().with_param("site_notices"));
    }
    // 清理任务按 `(value #>> '{}')::bigint` 读：null / 小数 / 字符串会让它每轮报错且永不清理
    if req.key == "retention_months" && req.value.as_u64().is_none_or(|months| months > 1200) {
        return Err(AppError::bad_request().with_param("retention_months"));
    }
    if is_pricing_base && okapi_kernel::pricing_loader::valid_base_price(&req.value).is_none() {
        return Err(AppError::bad_request().with_param("pricing_base_per_1m_micro"));
    }
    let is_timezone = req.key == okapi_store::timezone::SETTING;
    if is_timezone {
        validate_reporting_timezone(&state, &req.value).await?;
    }
    okapi_store::admin::set_setting(&state.pg, &req.key, &req.value, actor.actor_user_id()).await?;
    state.invalidate_routing_caches();
    // settings 热路径缓存按键失效：同进程立即生效（公告发布、限流阈值调整不必等 60s TTL），
    // 多副本靠 TTL 收敛——与鉴权缓存"本机即时、跨副本 TTL"同一取舍
    state.settings_cache.invalidate(&req.key).await;
    if is_timezone && okapi_store::timezone::apply_setting(Some(&req.value)) {
        // 本进程立即生效；别的副本由 routing.invalidate 广播 / 15s 轮询跟上
        state.stats_query_cache.invalidate_all();
    }
    audit(
        &state,
        &actor,
        "settings.set",
        &req.key,
        if super::super::manage::is_secret_key(&req.key) {
            json!({ "redacted": true, "configured": !req.value.is_null() })
        } else {
            json!({ "value": super::super::manage::redact_secret_fields(&req.value) })
        },
    )
    .await;
    Ok(Json(
        json!({ "ok": true, "requires_publish": is_pricing_base }),
    ))
}

/// 统计时区：null / 空串 = 跟随服务器；否则必须是 PG 与 ClickHouse 都认识的 IANA 名字——
/// 两边任一不认识，之后每条统计查询（会话时区）都会失败。名字会拼进 ClickHouse SQL，
/// 格式校验（`parse_setting`：只收 IANA 字符集）在任何查询之前。
async fn validate_reporting_timezone(state: &AppState, value: &Value) -> Result<(), AppError> {
    let invalid = || AppError::bad_request().with_param(okapi_store::timezone::SETTING);
    let Some(zone) = okapi_store::timezone::parse_setting(value).map_err(|()| invalid())? else {
        return Ok(());
    };
    let known: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_timezone_names WHERE name = $1)")
            .bind(&zone)
            .fetch_one(&state.pg)
            .await
            .map_err(|error| AppError::from(okapi_store::StoreError::from(error)))?;
    if !known {
        return Err(invalid());
    }
    // 只收与 UTC 差整小时的时区（含夏令时期间）：按小时汇总的历史桶按 UTC 整点切，半小时 / 45 分钟
    // 时差的时区（Asia/Kolkata、Asia/Kathmandu、Australia/Adelaide…）切不出精确的本地日界，
    // 统计查询会按 `statistics_calendar_history_incomplete` 拒绝。前后各一年按周取样，覆盖夏令时。
    let whole_hours: bool = sqlx::query_scalar(
        "SELECT bool_and(mod(extract(epoch FROM (t AT TIME ZONE $1) - (t AT TIME ZONE 'UTC'))::bigint, 3600) = 0) \
           FROM generate_series(now() - interval '1 year', now() + interval '1 year', interval '7 days') AS t",
    )
    .bind(&zone)
    .fetch_one(&state.pg)
    .await
    .map_err(|error| AppError::from(okapi_store::StoreError::from(error)))?;
    if !whole_hours {
        return Err(AppError::bad_request().with_param("reporting_timezone_whole_hours"));
    }
    if let Some(ch) = state.ch.as_ref() {
        ch.query_json_each_row(&format!(
            "SELECT toString(toTimeZone(now(), '{zone}')) AS local_now"
        ))
        .await
        .map_err(|_| invalid())?;
    }
    Ok(())
}

/// GET /admin/settings/{key}：单键读取（配置页回显；写权限同门槛防低权限窥探敏感配置）。
pub async fn get_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Json<Value>, AppError> {
    let _ = guard(&state, &headers, permissions::SETTINGS_WRITE).await?;
    let value = state.setting_fresh(&key).await?;
    if key == okapi_store::timezone::SETTING {
        // 配置页要显示「跟随服务器」时服务器是哪个时区，以及本进程实际在用的时区
        return Ok(Json(json!({
            "key": key,
            "value": value,
            "host_timezone": okapi_store::timezone::machine_timezone().ok(),
            "effective_timezone": okapi_store::timezone::reporting_timezone().ok(),
        })));
    }
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
    let mailer = okapi_kernel::mail::Mailer::from_pg(&state.pg)
        .await
        .map_err(|error| match error {
            okapi_kernel::mail::MailError::NotConfigured => {
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
    let lang = okapi_kernel::mail::templates::Lang::resolve(req.lang.as_deref(), accept);
    let site = state
        .setting_cached("site_name")
        .await
        .as_ref()
        .as_ref()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "Okapi".to_owned());
    if let Err(err) = mailer
        .send(okapi_kernel::mail::templates::test_message(lang, &site, to))
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
