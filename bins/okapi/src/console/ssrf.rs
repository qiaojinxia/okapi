//! 上游 URL SSRF 校验（IMPLEMENTATION §14.4，Sub2API url_allowlist 吸收）：
//! 管理面写入 channels.api_base 时执行——scheme 默认仅 https、
//! 目标默认禁私网/环回/链路本地（IP 字面量层面；DNS rebinding 深化列 backlog，
//! 配合出口 egress 白名单）。内网上游场景经 settings.ssrf_policy 放开：
//! `{"allow_http": bool, "allow_private": bool}`。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use serde::Deserialize;
use std::net::IpAddr;

#[derive(Deserialize, Default)]
pub struct SsrfPolicy {
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default)]
    pub allow_private: bool,
}

async fn load_policy(state: &AppState) -> SsrfPolicy {
    sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'ssrf_policy'"#)
        .fetch_optional(&state.pg)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

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

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 ULA 与 fe80::/10 链路本地
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// 校验 api_base；violation 返回 400（error_code 带原因参数）。
pub async fn validate_api_base(state: &AppState, api_base: &str) -> Result<(), AppError> {
    let policy = load_policy(state).await;
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
    let Some(host) = host_of(&lower) else {
        return Err(AppError::bad_request().with_param("api_base_host"));
    };
    if host.is_empty() {
        return Err(AppError::bad_request().with_param("api_base_host"));
    }
    if !policy.allow_private
        && let Ok(ip) = host.parse::<IpAddr>()
        && is_private_ip(ip)
    {
        return Err(AppError::bad_request().with_param("api_base_private_target"));
    }
    if !policy.allow_private && (host == "localhost" || host.ends_with(".internal")) {
        return Err(AppError::bad_request().with_param("api_base_private_target"));
    }
    Ok(())
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
