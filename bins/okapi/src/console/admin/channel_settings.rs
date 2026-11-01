//! 渠道 `settings` 的写入校验（API 版本、出口、模拟、额外字段、账号控制、OAuth 地址、Azure / Vertex 地址形状）。
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use serde_json::Value;

/// `settings.api_version`（azure 数据面版本）形状校验：`YYYY-MM-DD` 或 `YYYY-MM-DD-preview`。
/// 它会原样进每个请求的查询串，错一个字符整条渠道 404，所以在写入时就拦。
pub(super) fn ensure_settings_api_version(settings: Option<&Value>) -> Result<(), AppError> {
    let Some(v) = settings.and_then(|s| s.get("api_version")) else {
        return Ok(());
    };
    let ok = v.as_str().is_some_and(|s| {
        if !s.is_ascii() || s.len() < 10 {
            return false;
        }
        let (date, suffix) = s.split_at(10);
        date.len() == 10
            && date.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
            && (suffix.is_empty() || suffix == "-preview")
    });
    if ok {
        Ok(())
    } else {
        Err(AppError::bad_request().with_param("api_version"))
    }
}

/// `settings.extra_headers` 等出站修饰的写入校验（IMPLEMENTATION §11.30）；出口代理改由
/// 出口绑定表达（§11.41），settings 里不再接受 `proxy_url`。
pub(super) fn ensure_settings_outbound(settings: Option<&Value>) -> Result<(), AppError> {
    let Some(s) = settings else {
        return Ok(());
    };
    if let Some(value) = s.get("image_stream_usage")
        && !matches!(value.as_str(), Some("cumulative" | "per_image"))
    {
        return Err(AppError::bad_request().with_param("image_stream_usage"));
    }
    // 已退役（§11.41 出口绑定取代 §11.30 的 settings.proxy_url）：旧客户端写进来只会被静默
    // 忽略、请求悄悄直连，所以直接拒绝
    if s.get("proxy_url").is_some() {
        return Err(AppError::bad_request().with_param("proxy_url"));
    }
    if let Some(v) = s.get("extra_headers")
        && !okapi_providers::http::extra_headers_ok(v)
    {
        return Err(AppError::bad_request().with_param("extra_headers"));
    }
    if let Some(v) = s.get("inject_request_fields")
        && !inject_request_fields_ok(v)
    {
        return Err(AppError::bad_request().with_param("inject_request_fields"));
    }
    // aws_region（bedrock SigV4 区域覆写）：`us-east-1` 这类小写字母 / 数字 / 连字符
    if let Some(v) = s.get("aws_region")
        && !v.as_str().is_some_and(|r| {
            !r.is_empty()
                && r.len() <= 32
                && r.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
    {
        return Err(AppError::bad_request().with_param("aws_region"));
    }
    Ok(())
}

/// 已退役的 Claude Code 伪装开关（2026-10-05 起由 `extensions.client_profile` 取代）：
/// 旧前端写进来只会被静默忽略，所以直接拒绝。
pub(super) fn ensure_settings_mimic(settings: Option<&Value>) -> Result<(), AppError> {
    let Some(s) = settings else {
        return Ok(());
    };
    for key in ["mimic_cc", "mimic_cc_version"] {
        if s.get(key).is_some() {
            return Err(AppError::bad_request().with_param(key));
        }
    }
    Ok(())
}

/// `tool_loss_policy`：跨方言转换会丢工具时怎么办，只认 safe / strict / allow（缺省 safe）。
pub(super) fn ensure_settings_tool_loss_policy(settings: Option<&Value>) -> Result<(), AppError> {
    match settings.and_then(|s| s.get("tool_loss_policy")) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(value))
            if okapi_providers::convert::tool_loss::ToolLossPolicy::VALUES
                .contains(&value.as_str()) =>
        {
            Ok(())
        }
        Some(_) => Err(AppError::bad_request().with_param("tool_loss_policy")),
    }
}

pub(super) fn ensure_request_extensions(
    provider: &str,
    settings: Option<&Value>,
) -> Result<(), AppError> {
    let extensions = settings
        .and_then(|settings| settings.get("extensions"))
        .unwrap_or(&Value::Null);
    okapi_providers::profiles::validate_extensions(provider, extensions)
        .map_err(|_| AppError::bad_request().with_param("extensions"))
}

pub(in crate::console) fn ensure_account_control(settings: Option<&Value>) -> Result<(), AppError> {
    if let Some(settings) = settings {
        crate::gateway::account_control::policy::Policy::parse(settings)
            .map_err(|param| AppError::bad_request().with_param(param))?;
    }
    Ok(())
}

pub(in crate::console) async fn validate_channel_settings(
    state: &AppState,
    provider: &str,
    settings: Option<&Value>,
) -> Result<(), AppError> {
    if settings.is_some_and(|s| !s.is_object()) {
        return Err(AppError::bad_request().with_param("settings"));
    }
    ensure_settings_api_version(settings)?;
    ensure_settings_outbound(settings)?;
    ensure_settings_oauth_token_url(state, settings).await?;
    ensure_settings_mimic(settings)?;
    ensure_settings_tool_loss_policy(settings)?;
    ensure_request_extensions(provider, settings)?;
    ensure_account_control(settings)
}

/// `settings.oauth_token_url`（订阅 OAuth 的 token 端点覆写）与 api_base 过同一道 SSRF 闸：
/// 网关刷新 token 时会把 refresh token POST 到这个地址，OAuth 登录端点写它时已校验，
/// 但渠道设置的通用写入口此前没有——改一下 JSON 就能把刷新请求指向私网 / 元数据地址。
pub(super) async fn ensure_settings_oauth_token_url(
    state: &AppState,
    settings: Option<&Value>,
) -> Result<(), AppError> {
    let Some(v) = settings.and_then(|s| s.get("oauth_token_url")) else {
        return Ok(());
    };
    if v.is_null() {
        return Ok(());
    }
    let url = v
        .as_str()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| AppError::bad_request().with_param("oauth_token_url"))?;
    crate::gateway::ssrf::validate_api_base(state, url).await
}

/// `inject_request_fields`：对象、最多 32 键 / 4KB，受保护键不可写。
pub(super) fn inject_request_fields_ok(value: &Value) -> bool {
    // 与网关注入时跳过的键（`gateway::chat::adjust_request_fields`）同一份：少两键时能存、却静默不生效
    const PROTECTED: &[&str] = &[
        "model",
        "messages",
        "stream",
        "provider",
        "previous_response_id",
        "conversation",
    ];
    let Some(obj) = value.as_object() else {
        return false;
    };
    if obj.len() > 32 {
        return false;
    }
    if serde_json::to_vec(value)
        .ok()
        .is_none_or(|b| b.len() > 4096)
    {
        return false;
    }
    obj.keys()
        .all(|k| !k.is_empty() && !PROTECTED.contains(&k.as_str()))
}

/// azure / bedrock / vertex 渠道必须有地址：三家都没有全局缺省，缺了只能拼出打向
/// api.openai.com 的错误请求。vertex 还要求带 `/projects/{p}/locations/{l}`（§11.35）。
/// `provider` 是本次生效的协议，`api_base` 是本次生效的地址。
pub(in crate::console) fn ensure_azure_api_base(
    provider: &str,
    api_base: Option<&str>,
) -> Result<(), AppError> {
    let base = api_base.map(str::trim).filter(|b| !b.is_empty());
    match provider {
        "azure" | "bedrock" if base.is_none() => {
            Err(AppError::bad_request().with_param("api_base"))
        }
        "vertex" if !base.is_some_and(okapi_providers::vertex::api_base_ok) => {
            Err(AppError::bad_request().with_param("api_base"))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod request_extension_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn channel_configuration_checks_registered_extension_support() {
        let settings =
            json!({"extensions":{"client_profile":{"name":"claude-code","mode":"auto"}}});
        assert!(ensure_request_extensions("anthropic", Some(&settings)).is_ok());
        assert!(ensure_request_extensions("anthropic_max", Some(&settings)).is_ok());
        assert!(ensure_request_extensions("codex", Some(&settings)).is_err());
        assert!(ensure_request_extensions("openai", Some(&settings)).is_err());
        assert!(ensure_request_extensions("openai", None).is_ok());
    }
}

#[cfg(test)]
mod unicode_settings_tests {
    #[test]
    fn malformed_unicode_api_version_is_rejected_without_panicking() {
        for value in ["aaaaaaaaaé", "2026-10-03é", "界界界界"] {
            assert!(
                super::ensure_settings_api_version(Some(&serde_json::json!({"api_version":value})))
                    .is_err()
            );
        }
    }
}
