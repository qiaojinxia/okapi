//! 上游 HTTP 连接池：缺省 client + 按 `proxy_url` 缓存的出站代理 client。
//!
//! reqwest 的代理绑在 Client 上、不能按请求切换，所以每个不同的代理 URL 各自
//! 一个 Client（连接池独立）。渠道数通常个位数，缓存用 `RwLock<HashMap>` 足够，
//! 不引新依赖。`extra_headers` 是请求级的，在鉴权头之前写入——鉴权头后写覆盖，
//! 站长填了 `Authorization` 也换不掉渠道凭证。

use crate::error::UpstreamError;
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 一条渠道的出站修饰：代理 + 额外请求头。缺省 = 直连、不加头。
#[derive(Debug, Clone, Default)]
pub struct Outbound {
    pub proxy_url: Option<String>,
    pub extra_headers: Vec<(String, String)>,
}

impl Outbound {
    /// 从 `channels.settings` 抽出两键；形状不对当缺省（写入路径已经拦过）。
    #[must_use]
    pub fn from_settings(settings: &Value) -> Self {
        Self {
            proxy_url: proxy_url_from_settings(settings),
            extra_headers: extra_headers_from_settings(settings),
        }
    }
}

/// 从 settings 取 `proxy_url`（空串 = 未配）。
#[must_use]
pub fn proxy_url_from_settings(settings: &Value) -> Option<String> {
    settings
        .get("proxy_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 从 settings 取 `extra_headers`（对象 string→string；其它类型忽略）。
#[must_use]
pub fn extra_headers_from_settings(settings: &Value) -> Vec<(String, String)> {
    settings
        .get("extra_headers")
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// 管理面写入校验：`proxy_url` 空/缺省放行；非空必须是 http/https/socks5/socks5h 且带 host。
#[must_use]
pub fn proxy_url_ok(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.is_empty() || parse_proxy_url(trimmed).is_some()
}

/// 管理面写入校验：缺省/null 放行；必须是对象，键值都是非空字符串，键不是受保护头。
#[must_use]
pub fn extra_headers_ok(value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.iter().all(|(name, val)| {
        val.as_str().is_some_and(|v| {
            !name.trim().is_empty()
                && !is_forbidden_header(name)
                && HeaderName::from_bytes(name.as_bytes()).is_ok()
                && HeaderValue::from_str(v).is_ok()
        })
    })
}

fn parse_proxy_url(raw: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(raw).ok()?;
    matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
        .then_some(url)
        .filter(|u| u.host().is_some())
}

/// 不能让站长覆盖的头：鉴权、逐跳、Host、我们自己的请求 id。
/// 大小写不敏感。
#[must_use]
pub fn is_forbidden_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "host"
            | "content-length"
            | "content-type"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "upgrade"
            | "te"
            | "trailer"
            | "api-key"
            | "x-api-key"
            | "x-goog-api-key"
            | "x-okapi-request-id"
    )
}

/// 缺省 client + 按代理 URL 缓存的 client。`Clone` 共享同一份缓存。
///
/// 两族 client：数据面转发用的（跟随重定向，reqwest 缺省）与管理面探针用的（**不跟随**）。
/// SSRF 闸（`console::ssrf`）只校验管理员填进来的那个 URL，跟着 30x 走就能被一个公网地址
/// 引到私网 / 云元数据地址；测活、拉模型、余额、OAuth 换 token、Turnstile、支付回调这些
/// 外呼都没有跟随重定向的正当理由。数据面保留缺省：`/videos/{id}/content` 这类下载透传
/// 可能就靳上游 302 到 CDN。
#[derive(Clone)]
pub struct HttpPool {
    default: reqwest::Client,
    proxied: std::sync::Arc<RwLock<HashMap<String, reqwest::Client>>>,
    probe_default: reqwest::Client,
    probe_proxied: std::sync::Arc<RwLock<HashMap<String, reqwest::Client>>>,
}

impl HttpPool {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            default: build_client(None, true)?,
            proxied: std::sync::Arc::new(RwLock::new(HashMap::new())),
            probe_default: build_client(None, false)?,
            probe_proxied: std::sync::Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// 取出站 client：无代理用缺省池；有代理按 URL 缓存（锁中毒则当场再建，不 panic）。
    pub fn client(&self, proxy_url: Option<&str>) -> Result<reqwest::Client, UpstreamError> {
        cached_client(&self.default, &self.proxied, proxy_url, true)
    }

    /// 管理面探针 client：同样按代理缓存，但不跟随重定向。
    pub fn probe_client(&self, proxy_url: Option<&str>) -> Result<reqwest::Client, UpstreamError> {
        cached_client(&self.probe_default, &self.probe_proxied, proxy_url, false)
    }

    /// 管理面探针请求（与 `request` 同形，换用不跟随重定向的 client）。
    pub fn probe(
        &self,
        outbound: &Outbound,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        let client = self.probe_client(outbound.proxy_url.as_deref())?;
        Ok(apply_extra_headers(
            client.request(method, url),
            &outbound.extra_headers,
        ))
    }

    pub fn post(
        &self,
        outbound: &Outbound,
        url: impl reqwest::IntoUrl,
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        self.request(outbound, reqwest::Method::POST, url)
    }

    pub fn get(
        &self,
        outbound: &Outbound,
        url: impl reqwest::IntoUrl,
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        self.request(outbound, reqwest::Method::GET, url)
    }

    pub fn request(
        &self,
        outbound: &Outbound,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        let client = self.client(outbound.proxy_url.as_deref())?;
        Ok(apply_extra_headers(
            client.request(method, url),
            &outbound.extra_headers,
        ))
    }
}

fn cached_client(
    default: &reqwest::Client,
    cache: &RwLock<HashMap<String, reqwest::Client>>,
    proxy_url: Option<&str>,
    follow_redirects: bool,
) -> Result<reqwest::Client, UpstreamError> {
    let Some(url) = proxy_url.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(default.clone());
    };
    if let Ok(guard) = cache.read()
        && let Some(hit) = guard.get(url)
    {
        return Ok(hit.clone());
    }
    let built = build_client(Some(url), follow_redirects)?;
    if let Ok(mut guard) = cache.write() {
        guard.insert(url.to_owned(), built.clone());
    }
    Ok(built)
}

fn build_client(
    proxy_url: Option<&str>,
    follow_redirects: bool,
) -> Result<reqwest::Client, UpstreamError> {
    let mut builder = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT);
    if !follow_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    if let Some(raw) = proxy_url {
        let url =
            parse_proxy_url(raw).ok_or_else(|| UpstreamError::Build("proxy_url".to_owned()))?;
        let proxy = reqwest::Proxy::all(url)
            .map_err(|e| UpstreamError::Build(format!("proxy_url: {e}")))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

/// 先写额外头（跳过非法 / 受保护名），调用方随后写鉴权与 Content-Type。
pub fn apply_extra_headers(
    mut req: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if is_forbidden_header(name) {
            continue;
        }
        let Ok(n) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(v) = HeaderValue::from_str(value) else {
            continue;
        };
        req = req.header(n, v);
    }
    req
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn proxy_url_accepts_http_and_socks() {
        assert!(proxy_url_ok(""));
        assert!(proxy_url_ok("  "));
        assert!(proxy_url_ok("http://127.0.0.1:7890"));
        assert!(proxy_url_ok("https://proxy.example:8443"));
        assert!(proxy_url_ok("socks5://127.0.0.1:1080"));
        assert!(proxy_url_ok("socks5h://user:pass@127.0.0.1:1080"));
        assert!(!proxy_url_ok("ftp://127.0.0.1:21"));
        assert!(!proxy_url_ok("http://"));
        assert!(!proxy_url_ok("not-a-url"));
    }

    #[test]
    fn extra_headers_reject_forbidden_and_non_object() {
        assert!(extra_headers_ok(&Value::Null));
        assert!(extra_headers_ok(
            &json!({"X-Custom": "a", "OpenAI-Organization": "org"})
        ));
        assert!(!extra_headers_ok(&json!({"Authorization": "Bearer x"})));
        assert!(!extra_headers_ok(&json!({"authorization": "x"})));
        assert!(!extra_headers_ok(&json!({"api-key": "x"})));
        assert!(!extra_headers_ok(&json!({"X-Custom": 1})));
        assert!(!extra_headers_ok(&json!(["X-Custom"])));
        assert!(!extra_headers_ok(&json!({"": "x"})));
        assert!(!extra_headers_ok(&json!({"bad name": "x"})));
    }

    #[test]
    fn from_settings_reads_both_keys() {
        let o = Outbound::from_settings(&json!({
            "proxy_url": " http://127.0.0.1:9 ",
            "extra_headers": {"X-A": "1", "skip": 2}
        }));
        assert_eq!(o.proxy_url.as_deref(), Some("http://127.0.0.1:9"));
        assert_eq!(o.extra_headers, vec![("X-A".to_owned(), "1".to_owned())]);
        assert!(Outbound::from_settings(&json!({})).proxy_url.is_none());
    }
}
