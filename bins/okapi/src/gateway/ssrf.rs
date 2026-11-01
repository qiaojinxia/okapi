//! 上游 URL SSRF 校验（IMPLEMENTATION §14.4，Sub2API url_allowlist 吸收）：
//! 管理面写入 channels.api_base、`settings.oauth_token_url` 与 Vertex 服务账号 `token_uri`
//! 时执行，网关的图片批量绑定、视频下载跳转与 worker 的通知 webhook 也用同一套判据——目标默认禁私网/环回/链路本地（IP 字面量 + 保存时解析一次域名）。这是第一层；
//! 域名保存之后可以改指内网（DNS 重绑定），所以出站直连在**连接时**还会按同一判据再校验一次、
//! 并只连校验过的地址（第二层，`okapi_providers::ssrf`，策略由 `gateway::sync_ssrf_policy` 同步）。
//!
//! scheme 按地址的用途分两档（`settings.ssrf_policy`）：
//! - 渠道的 API 地址（[`validate_upstream_base`]）缺省**接受 http**——自建 / 转售上游常常只有一个明文端口；
//!   scheme 不是 SSRF 的防线（防的是私网目标，那道闸不变），明文传输的风险交给管理员自己取舍，
//!   `allow_http_upstream: false` 可以强制只收 https；
//! - 其余地址（令牌端点、回调、支付网关、探测地址……，[`validate_api_base`] / [`validate_url`]）缺省仅 https，
//!   `allow_http: true` 放开。
//!
//! 内网上游场景另需 `allow_private: true`。完整形状：
//! `{"allow_http": bool, "allow_http_upstream": bool（缺省 true）, "allow_private": bool}`。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use okapi_providers::ssrf::is_private_ip;
use serde::Deserialize;
use std::net::IpAddr;

#[derive(Deserialize, Default)]
pub struct SsrfPolicy {
    /// 渠道 API 地址以外的地址是否接受 http（缺省不接受）。
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default)]
    pub allow_private: bool,
    /// 渠道 API 地址是否接受 http；没写就是接受。
    #[serde(default)]
    pub allow_http_upstream: Option<bool>,
}

/// 地址的用途：决定 http 缺省放不放行。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// 渠道的 API 地址（`channels.api_base`）。
    UpstreamBase,
    /// 令牌端点、回调、支付网关、探测地址等。
    Other,
}

impl SsrfPolicy {
    fn http_allowed(&self, purpose: Purpose) -> bool {
        match purpose {
            Purpose::UpstreamBase => self.allow_http_upstream.unwrap_or(true),
            Purpose::Other => self.allow_http,
        }
    }
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

/// 校验渠道的 API 地址（`channels.api_base`）：缺省 http / https 都收，私网目标照旧要 `allow_private`。
/// violation 返回 400（error_code 带原因参数）。
pub async fn validate_upstream_base(state: &AppState, api_base: &str) -> Result<(), AppError> {
    validate_with(&state.pg, api_base, Purpose::UpstreamBase).await
}

/// 校验其余的出站地址（令牌端点、回调、支付网关、探测地址……）：缺省仅 https。
/// 名字沿用旧的；渠道 API 地址请走 [`validate_upstream_base`]。
pub async fn validate_api_base(state: &AppState, api_base: &str) -> Result<(), AppError> {
    validate_url(&state.pg, api_base).await
}

pub(crate) async fn validate_url(pg: &sqlx::PgPool, api_base: &str) -> Result<(), AppError> {
    validate_with(pg, api_base, Purpose::Other).await
}

async fn validate_with(
    pg: &sqlx::PgPool,
    api_base: &str,
    purpose: Purpose,
) -> Result<(), AppError> {
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
        if !policy.http_allowed(purpose) {
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

    /// http 缺省只对渠道 API 地址放行；其余地址要 `allow_http`；两个开关互不牵连。
    #[test]
    fn http_policy_is_decided_per_purpose() {
        let policy = |json: serde_json::Value| serde_json::from_value::<SsrfPolicy>(json).unwrap();
        let none = policy(serde_json::json!({}));
        assert!(
            none.http_allowed(Purpose::UpstreamBase),
            "渠道地址缺省接受 http"
        );
        assert!(!none.http_allowed(Purpose::Other), "其它地址缺省仅 https");
        // 老的策略行（只有 allow_http / allow_private）：渠道地址也接受 http——这是缺省的变化
        let legacy_strict =
            policy(serde_json::json!({"allow_http": false, "allow_private": false}));
        assert!(legacy_strict.http_allowed(Purpose::UpstreamBase));
        assert!(!legacy_strict.http_allowed(Purpose::Other));
        let legacy_open = policy(serde_json::json!({"allow_http": true, "allow_private": true}));
        assert!(legacy_open.http_allowed(Purpose::UpstreamBase));
        assert!(legacy_open.http_allowed(Purpose::Other));
        // 强制渠道地址只收 https，不影响其它地址
        let strict_upstream =
            policy(serde_json::json!({"allow_http": true, "allow_http_upstream": false}));
        assert!(!strict_upstream.http_allowed(Purpose::UpstreamBase));
        assert!(strict_upstream.http_allowed(Purpose::Other));
        // null 当没写
        let null = policy(serde_json::json!({"allow_http_upstream": null}));
        assert!(null.http_allowed(Purpose::UpstreamBase));
        let explicit = policy(serde_json::json!({"allow_http_upstream": true}));
        assert!(explicit.http_allowed(Purpose::UpstreamBase));
        assert!(!explicit.http_allowed(Purpose::Other));
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
