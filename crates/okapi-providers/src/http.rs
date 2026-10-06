//! 上游 HTTP 连接池：缺省 client + 按代理 URL 缓存的出站代理 client。
//!
//! reqwest 的代理绑在 Client 上、不能按请求切换，所以每个不同的代理 URL 各自
//! 一个 Client（连接池独立）。代理成为一等资源后（IMPLEMENTATION §11.41）数量可到
//! 上百个，且改密码 / 删代理会留下不再使用的 URL：缓存按闲置时间淘汰（新建 client 时顺手清），
//! 仍用 `RwLock<HashMap>`，不引新依赖。`extra_headers` 是请求级的，在鉴权头之前写入——
//! 鉴权头后写覆盖，站长填了 `Authorization` 也换不掉渠道凭证。

use crate::error::UpstreamError;
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 代理 client 闲置这么久就淘汰（连接池随之关闭）。
const PROXIED_IDLE: Duration = Duration::from_mins(15);

/// 一条渠道的出站修饰：代理 + 额外请求头。缺省 = 直连、不加头。
#[derive(Debug, Clone, Default)]
pub struct Outbound {
    pub proxy_url: Option<String>,
    pub extra_headers: Vec<(String, String)>,
    /// Opaque request extension context. HTTP transport never interprets it.
    pub context: crate::profiles::RequestContext,
}

impl Outbound {
    /// 从 `channels.settings` 抽额外头与扩展；形状不对当缺省（写入路径已经拦过）。
    /// 出口代理不在 settings 里（§11.41 出口绑定），由调用方按 key 解析后传入 `proxy_url`——
    /// 刻意不提供「从 settings 读代理」的捷径，免得某条路径漏解析而悄悄直连。
    #[must_use]
    pub fn from_settings(settings: &Value, proxy_url: Option<String>) -> Self {
        Self {
            proxy_url,
            extra_headers: extra_headers_from_settings(settings),
            context: crate::profiles::RequestContext {
                extensions: settings.get("extensions").cloned().unwrap_or_default(),
                ..Default::default()
            },
        }
    }
}

/// 代理 URL 的非密部分（展示 / 筛选用）。认证信息只留「有没有」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub has_password: bool,
}

impl ProxyEndpoint {
    /// 解析并校验代理 URL：http / https / socks5 / socks5h + host；端口缺省按 scheme
    /// （http 80 / https 443 / socks 1080，与 reqwest 实际连接的端口一致）。不接受路径、查询串。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let url = parse_proxy_url(raw.trim())?;
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return None;
        }
        let scheme = url.scheme().to_owned();
        let port = url.port().unwrap_or(match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => 1080,
        });
        let username = Some(url.username())
            .filter(|u| !u.is_empty())
            .map(percent_decode);
        Some(Self {
            host: url.host_str()?.to_owned(),
            scheme,
            port,
            username,
            has_password: url.password().is_some(),
        })
    }

    /// 掩码后的展示形态：`socks5h://user:***@host:1080`。
    #[must_use]
    pub fn masked(&self) -> String {
        let auth = match (&self.username, self.has_password) {
            (Some(user), true) => format!("{user}:***@"),
            (Some(user), false) => format!("{user}@"),
            (None, true) => ":***@".to_owned(),
            (None, false) => String::new(),
        };
        format!("{}://{auth}{}:{}", self.scheme, self.host, self.port)
    }
}

/// userinfo 的百分号解码（展示用，非法序列原样保留）。
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = raw.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
            | "sec-websocket-key"
            | "sec-websocket-accept"
            | "sec-websocket-version"
            | "sec-websocket-protocol"
            | "sec-websocket-extensions"
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
/// 数据面、管理面探针与 WebSocket 握手均不自动跟随重定向。
/// WS 单独限制为 HTTP/1：仅给请求设置 version 不会限制 TLS 的 ALPN 协商。
/// SSRF 闸（`console::ssrf`）只校验管理员填进来的那个 URL，跟着 30x 走就能被一个公网地址
/// 引到私网 / 云元数据地址；测活、拉模型、余额、Turnstile、支付回调、订阅 OAuth 换码 / 刷新、
/// Vertex 服务账号换 token、Bedrock 列模型这些外呼都不跟随重定向。
/// 视频下载的有限跳转由 gateway 逐跳校验目标并按 origin 移除凭证。
#[derive(Clone)]
pub struct HttpPool {
    clients: std::sync::Arc<Clients>,
}

struct Clients {
    default: reqwest::Client,
    proxied: RwLock<HashMap<String, Cached>>,
    probe_default: reqwest::Client,
    probe_proxied: RwLock<HashMap<String, Cached>>,
    websocket_default: reqwest::Client,
    websocket_proxied: RwLock<HashMap<String, Cached>>,
}

/// 代理 client + 最近一次取用时刻（相对进程内基准的秒数，读锁下也能更新）。
struct Cached {
    client: reqwest::Client,
    last_used: AtomicU64,
}

fn now_secs() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs()
}

#[derive(Clone, Copy)]
enum ClientPolicy {
    Forward,
    Probe,
    WebSocket,
}

impl HttpPool {
    pub fn new() -> Result<Self, UpstreamError> {
        Ok(Self {
            clients: std::sync::Arc::new(Clients {
                default: build_client(None, ClientPolicy::Forward)?,
                proxied: RwLock::new(HashMap::new()),
                probe_default: build_client(None, ClientPolicy::Probe)?,
                probe_proxied: RwLock::new(HashMap::new()),
                websocket_default: build_client(None, ClientPolicy::WebSocket)?,
                websocket_proxied: RwLock::new(HashMap::new()),
            }),
        })
    }

    /// 取出站 client：无代理用缺省池；有代理按 URL 缓存（锁中毒则当场再建，不 panic）。
    pub fn client(&self, proxy_url: Option<&str>) -> Result<reqwest::Client, UpstreamError> {
        cached_client(
            &self.clients.default,
            &self.clients.proxied,
            proxy_url,
            ClientPolicy::Forward,
        )
    }

    /// 管理面探针 client：同样按代理缓存，但不跟随重定向。
    pub fn probe_client(&self, proxy_url: Option<&str>) -> Result<reqwest::Client, UpstreamError> {
        cached_client(
            &self.clients.probe_default,
            &self.clients.probe_proxied,
            proxy_url,
            ClientPolicy::Probe,
        )
    }

    /// RFC 6455 upgrade：独立 HTTP/1-only 池，代理与 TLS 校验保持一致。
    pub(crate) fn websocket(
        &self,
        outbound: &Outbound,
        url: impl reqwest::IntoUrl,
    ) -> Result<reqwest::RequestBuilder, UpstreamError> {
        let client = cached_client(
            &self.clients.websocket_default,
            &self.clients.websocket_proxied,
            outbound.proxy_url.as_deref(),
            ClientPolicy::WebSocket,
        )?;
        Ok(apply_extra_headers(
            client.get(url),
            &outbound.extra_headers,
        ))
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
    cache: &RwLock<HashMap<String, Cached>>,
    proxy_url: Option<&str>,
    policy: ClientPolicy,
) -> Result<reqwest::Client, UpstreamError> {
    let Some(url) = proxy_url.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(default.clone());
    };
    if let Ok(guard) = cache.read()
        && let Some(hit) = guard.get(url)
    {
        hit.last_used.store(now_secs(), Ordering::Relaxed);
        return Ok(hit.client.clone());
    }
    let built = build_client(Some(url), policy)?;
    if let Ok(mut guard) = cache.write() {
        // 新建时顺手淘汰闲置的：改过密码 / 删掉的代理不会再被取用，留着只占连接池
        let now = now_secs();
        evict_idle(&mut guard, now);
        guard.insert(
            url.to_owned(),
            Cached {
                client: built.clone(),
                last_used: AtomicU64::new(now),
            },
        );
    }
    Ok(built)
}

fn evict_idle(cache: &mut HashMap<String, Cached>, now: u64) {
    cache.retain(|_, entry| {
        now.saturating_sub(entry.last_used.load(Ordering::Relaxed)) < PROXIED_IDLE.as_secs()
    });
}

fn build_client(
    proxy_url: Option<&str>,
    policy: ClientPolicy,
) -> Result<reqwest::Client, UpstreamError> {
    client_builder(proxy_url, policy)?
        .build()
        .map_err(|e| UpstreamError::Build(e.to_string()))
}

fn client_builder(
    proxy_url: Option<&str>,
    policy: ClientPolicy,
) -> Result<reqwest::ClientBuilder, UpstreamError> {
    // Custom auth headers are not stripped by reqwest on cross-host redirects.
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    if matches!(policy, ClientPolicy::WebSocket) {
        builder = builder.http1_only();
    }
    if let Some(raw) = proxy_url {
        let url =
            parse_proxy_url(raw).ok_or_else(|| UpstreamError::Build("proxy_url".to_owned()))?;
        let proxy = reqwest::Proxy::all(url)
            .map_err(|e| UpstreamError::Build(format!("proxy_url: {e}")))?;
        builder = builder.proxy(proxy);
    }
    Ok(builder)
}

#[cfg(test)]
#[path = "http_tls_tests.rs"]
mod tls_tests;

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

    #[tokio::test]
    async fn forward_never_follows_custom_credential_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = target.local_addr().unwrap();
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = origin.local_addr().unwrap();
        let serve = tokio::spawn(async move {
            let (mut socket, _) = origin.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let used = socket.read(&mut request).await.unwrap();
            assert!(used > 0);
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{address}/steal\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
        });
        let response = HttpPool::new()
            .unwrap()
            .post(&Outbound::default(), format!("http://{source}/api"))
            .unwrap()
            .header("x-api-key", "secret")
            .header("api-key", "secret")
            .header("x-goog-api-key", "secret")
            .header("x-amz-security-token", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 302);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), target.accept())
                .await
                .is_err()
        );
        serve.await.unwrap();
    }

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
        for header in [
            "Sec-WebSocket-Key",
            "sec-websocket-accept",
            "sec-websocket-version",
            "sec-websocket-protocol",
            "sec-websocket-extensions",
        ] {
            assert!(!extra_headers_ok(&json!({header: "override"})));
        }
        assert!(!extra_headers_ok(&json!({"X-Custom": 1})));
        assert!(!extra_headers_ok(&json!(["X-Custom"])));
        assert!(!extra_headers_ok(&json!({"": "x"})));
        assert!(!extra_headers_ok(&json!({"bad name": "x"})));
    }

    #[test]
    fn from_settings_never_reads_a_proxy_from_settings() {
        // 退役的 settings.proxy_url 残留也不能被当成出口：出口只认调用方解析好的绑定
        let o = Outbound::from_settings(
            &json!({
                "proxy_url": "http://127.0.0.1:9",
                "extra_headers": {"X-A": "1", "skip": 2}
            }),
            None,
        );
        assert!(o.proxy_url.is_none());
        assert_eq!(o.extra_headers, vec![("X-A".to_owned(), "1".to_owned())]);
        let o = Outbound::from_settings(&json!({}), Some("socks5h://p:1080".into()));
        assert_eq!(o.proxy_url.as_deref(), Some("socks5h://p:1080"));
    }

    #[test]
    fn proxy_endpoint_parses_defaults_and_masks_credentials() {
        let e = ProxyEndpoint::parse(" socks5h://us%40er:p%40ss@10.0.0.1 ").unwrap();
        assert_eq!(
            (e.scheme.as_str(), e.host.as_str(), e.port),
            ("socks5h", "10.0.0.1", 1080)
        );
        assert_eq!(e.username.as_deref(), Some("us@er"));
        assert!(e.has_password);
        assert_eq!(e.masked(), "socks5h://us@er:***@10.0.0.1:1080");
        assert_eq!(ProxyEndpoint::parse("http://h").unwrap().port, 80);
        assert_eq!(ProxyEndpoint::parse("https://h").unwrap().port, 443);
        let v6 = ProxyEndpoint::parse("http://[::1]:7890/").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("[::1]", 7890));
        assert_eq!(v6.masked(), "http://[::1]:7890");
        for bad in [
            "",
            "ftp://h:21",
            "http://",
            "h:8080",
            "http://h:8080/path",
            "http://h:8080/?q=1",
        ] {
            assert!(ProxyEndpoint::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn idle_proxied_clients_are_evicted() {
        let client = build_client(None, ClientPolicy::Forward).unwrap();
        let idle = PROXIED_IDLE.as_secs();
        let mut cache = HashMap::from([
            (
                "http://old:1".to_owned(),
                Cached {
                    client: client.clone(),
                    last_used: AtomicU64::new(0),
                },
            ),
            (
                "http://fresh:2".to_owned(),
                Cached {
                    client,
                    last_used: AtomicU64::new(idle),
                },
            ),
        ]);
        evict_idle(&mut cache, idle + 1);
        assert_eq!(cache.keys().collect::<Vec<_>>(), ["http://fresh:2"]);
    }
}
