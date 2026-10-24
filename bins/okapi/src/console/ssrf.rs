//! 上游 URL SSRF 校验（IMPLEMENTATION §14.4，Sub2API url_allowlist 吸收）：
//! 管理面写入 channels.api_base、`settings.oauth_token_url` 与 Vertex 服务账号 `token_uri`
//! 时执行——scheme 默认仅 https、
//! 目标默认禁私网/环回/链路本地（IP 字面量 + 保存时解析一次域名）。这是第一层；
//! 域名保存之后可以改指内网（DNS 重绑定），所以出站直连在**连接时**还会按同一判据再校验一次、
//! 并只连校验过的地址（第二层，`okapi_providers::ssrf`，策略由 `gateway::sync_ssrf_policy` 同步）。
//! 内网上游场景经 settings.ssrf_policy 放开：`{"allow_http": bool, "allow_private": bool}`。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use okapi_providers::ssrf::is_private_ip;
use serde::Deserialize;
use std::net::IpAddr;

#[derive(Deserialize, Default)]
pub struct SsrfPolicy {
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default)]
    pub allow_private: bool,
}

async fn load_policy(pg: &sqlx::PgPool) -> SsrfPolicy {
    sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'ssrf_policy'"#)
        .fetch_optional(pg)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

#[cfg(test)]
fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // IPv6 字面量 [::1]:8080
    if let Some(stripped) = authority.strip_prefix('[') {
        return stripped.split(']').next();
    }
    Some(authority.split(':').next().unwrap_or(authority))
}

/// 校验 api_base；violation 返回 400（error_code 带原因参数）。
pub async fn validate_api_base(state: &AppState, api_base: &str) -> Result<(), AppError> {
    validate_url(&state.pg, api_base).await
}

pub(crate) async fn validate_url(pg: &sqlx::PgPool, api_base: &str) -> Result<(), AppError> {
    let policy = load_policy(pg).await;
    let parsed = reqwest::Url::parse(api_base)
        .map_err(|_| AppError::bad_request().with_param("api_base_host"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::bad_request().with_param("api_base_host"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::bad_request().with_param("api_base_credentials"));
    }
    let lower = api_base.trim().to_lowercase();
    if lower.starts_with("https://") {
        // scheme ok
    } else if lower.starts_with("http://") {
        if !policy.allow_http {
            return Err(AppError::bad_request().with_param("api_base_scheme_https_only"));
        }
    } else {
        return Err(AppError::bad_request().with_param("api_base_scheme"));
    }
    let host = host.trim_matches(['[', ']']);
    if !policy.allow_private
        && let Ok(ip) = host.parse::<IpAddr>()
        && is_private_ip(ip)
    {
        return Err(AppError::bad_request().with_param("api_base_private_target"));
    }
    if !policy.allow_private
        && (host == "localhost" || host.ends_with(".localhost") || host.ends_with(".internal"))
    {
        return Err(AppError::bad_request().with_param("api_base_private_target"));
    }
    // 域名也看解析结果：公网域名解析到 169.254.169.254 / 内网段同样能打到内网。保存配置时
    // 解析一次；解析失败不拦（运行时同样连不上）。保存之后域名再改指内网（DNS 重绑定）由出站连接时的
    // 同一判据拦（`okapi_providers::ssrf`）。
    if !policy.allow_private && host.parse::<IpAddr>().is_err() {
        let port = parsed.port_or_known_default().unwrap_or(443);
        let resolved = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::net::lookup_host((host, port)),
        )
        .await;
        if let Ok(Ok(mut addrs)) = resolved
            && addrs.any(|addr| is_private_ip(addr.ip()))
        {
            return Err(AppError::bad_request().with_param("api_base_private_target"));
        }
    }
    Ok(())
}

/// 凭证里藏着的出站地址也过闸：Vertex 服务账号 JSON 的 `token_uri`——网关换 access token 时
/// 把 JWT 断言 POST 到它，测活还会把非 2xx 响应体前 300 字回给管理员。凭证密封存储，写入口是
/// 唯一能看见它的地方；按凭证形状判断而不按 provider，先以别的协议存下再 PATCH 成 vertex 也
/// 绕不过去。违规统一回 `credential_token_uri`（管理员没动 api_base，不该看到 api_base 的参数名）。
pub async fn validate_credential(state: &AppState, credential: &str) -> Result<(), AppError> {
    let Some(sa) = okapi_providers::vertex::ServiceAccount::parse(credential) else {
        return Ok(());
    };
    validate_api_base(state, &sa.token_uri)
        .await
        .map_err(|_| AppError::bad_request().with_param("credential_token_uri"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_extraction() {
        assert_eq!(
            host_of("https://api.example.com/v1"),
            Some("api.example.com")
        );
        assert_eq!(host_of("http://127.0.0.1:8080/v1"), Some("127.0.0.1"));
        assert_eq!(host_of("http://[::1]:9/v1"), Some("::1"));
        assert_eq!(host_of("https://u:p@evil.com/x"), Some("evil.com"));
    }

    #[test]
    fn private_ranges() {
        for ip in [
            "10.0.0.1",
            "172.16.5.5",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "0.0.0.0",
            // 阿里云元数据地址落在共享地址空间 100.64.0.0/10
            "100.100.100.200",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["::1", "fc00::1", "fe80::1"] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("2606:4700::1".parse().unwrap()));
    }
}
