//! Claude Code 客户端外形的出站连接（IMPLEMENTATION §11.42）。
//!
//! 请求头与请求体已逐字对齐真机，剩下最容易被区分的是 TLS 握手：reqwest + rustls 的
//! ClientHello（JA4 `t13d1011h2_…`，ALPN 含 h2）与真机 CLI 完全不同。CLI 是 Bun 打包的，
//! TLS 走 Bun 内置的 BoringSSL；这里用同一个库按 2.1.293 第一方抓包配置，`/v1/messages`
//! 那条连接的 ClientHello 逐字相同：JA3 `1523504b38f0fae0d881d4b6554aac1b`、
//! JA4 `t13d1713h1_5b57614c22b0_6a3d802a7139`（17 个套件含 CBC 老套件、13 个扩展按 BoringSSL
//! 原生顺序、无 GREASE、组含 X25519MLKEM768、ALPN 只有 http/1.1）。
//!
//! reqwest 不能换 TLS 实现，于是只换「发送」：请求照常用 reqwest 构造，转成 `http::Request`
//! 交给 hyper 的 HTTP/1.1 连接池（连接器做 BoringSSL 握手，出口代理的隧道也自己建），
//! 响应再转回 `reqwest::Response`——下游的流式解析、错误体读取一行不改。
//!
//! 真机的账号接口（换码、刷新、`/api/...`）走 axios，握手是另一套：JA3
//! `5355e3851d76d069ab8a98fdb51cf1b4`、JA4 `t13d181000_5d04281c6031_78e6aca7449b`（18 个套件、套件顺序不同、
//! 无 ALPN / OCSP / SCT 扩展），见 [`Shape::Account`]。
//!
//! hyper 写 HTTP/1 头名一律小写，真机按调用方的写法原样发（`Content-Type`、`X-Stainless-OS` 与
//! `anthropic-version` 并存）；连接层把头名改回真机拼写（[`HeadCase`]）。`/v1/messages` 的头顺序、
//! `Connection` / `Accept-Encoding` 照真机补齐重排（[`arrange_messages`]），压缩的响应在这里解开（[`decode`]）。
use crate::UpstreamError;
use base64::Engine as _;
use boring::ssl::{SslConnector, SslMethod, SslVersion};
use bytes::Bytes;
use http::Uri;
use hyper_util::client::legacy::{
    Client,
    connect::{Connected, Connection},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// TLS 1.2 套件按真机顺序（TLS 1.3 的三个由 BoringSSL 固定在最前，顺序与真机相同）。
const CIPHERS: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:\
ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:\
ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES128-SHA:ECDHE-RSA-AES128-SHA:ECDHE-ECDSA-AES256-SHA:\
ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA";
/// 账号接口那套的 TLS 1.2 套件（多一个 `ECDHE-RSA-AES128-SHA256`，RSA 在 ECDSA 前）。
const ACCOUNT_CIPHERS: &str = "ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES128-GCM-SHA256:\
ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES128-SHA256:\
ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES128-SHA:ECDHE-RSA-AES128-SHA:\
ECDHE-ECDSA-AES256-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA";
const CURVES: &str = "X25519MLKEM768:X25519:P-256:P-384";
/// 真机发出时不是全小写的头名（2.1.293 第一方抓包）；不在表里的照 hyper 的小写发，
/// 与真机的 `anthropic-*`、`x-app`、`x-claude-code-prompt-id` 等一致。
const WIRE_NAMES: [&str; 19] = [
    "Accept",
    "Accept-Encoding",
    "Authorization",
    "Cache-Control",
    "Connection",
    "Content-Length",
    "Content-Type",
    "Host",
    "Pragma",
    "User-Agent",
    "X-Claude-Code-Session-Id",
    "X-Stainless-Arch",
    "X-Stainless-Lang",
    "X-Stainless-OS",
    "X-Stainless-Package-Version",
    "X-Stainless-Retry-Count",
    "X-Stainless-Runtime",
    "X-Stainless-Runtime-Version",
    "X-Stainless-Timeout",
];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POOL_IDLE: Duration = Duration::from_secs(90);
/// 连接池句柄闲置多久从表里摘掉（池里的连接本身 90 秒就关）。
const POOL_ENTRY_IDLE: Duration = Duration::from_mins(15);
const MAX_PROXY_HEAD: usize = 8 * 1024;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type BoxIo = Box<dyn Io>;

type Pool = Client<Connector, reqwest::Body>;

/// `/v1/messages` 上真机的头顺序（2.1.293 第一方抓包，主请求、起标题请求、sdk-cli 三份一致）。
/// 不在表里的（渠道额外头等）排在 `x-client-request-id` 之后、`connection` 之前。
const MESSAGES_ORDER: [&str; 24] = [
    "accept",
    "authorization",
    "content-type",
    "user-agent",
    "x-claude-code-session-id",
    "x-stainless-arch",
    "x-stainless-lang",
    "x-stainless-os",
    "x-stainless-package-version",
    "x-stainless-retry-count",
    "x-stainless-runtime",
    "x-stainless-runtime-version",
    "x-stainless-timeout",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "anthropic-version",
    "x-app",
    "x-claude-code-prompt-id",
    "x-claude-code-request-class",
    "x-client-request-id",
    "connection",
    "host",
    "accept-encoding",
    "content-length",
];
/// 未知头紧跟在这一项之后。
const MESSAGES_UNKNOWN_AFTER: &str = "x-client-request-id";
/// 真机（Bun 的 fetch）在这条连接上自己补的两个头。
const MESSAGES_CONNECTION: &str = "keep-alive";
const MESSAGES_ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

/// 真机的两套握手：模型请求走 Anthropic SDK，账号接口走 axios。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Messages,
    Account,
}

/// 连接池按（账号, 出口代理）分开：真机一个进程只有一个账号，一条 keep-alive 连接上
/// 绝不会先后出现两个账号的 token。闲置的池与 `HttpPool` 的 reqwest 池一样按时淘汰。
pub(crate) struct ClaudeCodeTls {
    shape: Shape,
    ssl: SslConnector,
    pools: RwLock<HashMap<PoolKey, (Pool, AtomicU64)>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    account: Option<String>,
    proxy: Option<String>,
}

impl ClaudeCodeTls {
    pub(crate) fn new(shape: Shape) -> Result<Self, UpstreamError> {
        Ok(Self {
            shape,
            ssl: ssl_connector(shape, None)?,
            pools: RwLock::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_root(shape: Shape, root_der: &[u8]) -> Result<Self, UpstreamError> {
        Ok(Self {
            shape,
            ssl: ssl_connector(shape, Some(root_der))?,
            pools: RwLock::new(HashMap::new()),
        })
    }

    fn pool(&self, account: Option<&str>, proxy_url: Option<&str>) -> Result<Pool, UpstreamError> {
        let key = PoolKey {
            account: account.map(str::to_owned),
            proxy: proxy_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        };
        let now = crate::http::now_secs();
        if let Ok(guard) = self.pools.read()
            && let Some((pool, last_used)) = guard.get(&key)
        {
            last_used.store(now, Ordering::Relaxed);
            return Ok(pool.clone());
        }
        let hop = match key.proxy.as_deref() {
            Some(url) => Hop::parse(url)?,
            None => Hop::Direct,
        };
        let Ok(mut guard) = self.pools.write() else {
            return Ok(pool(&self.ssl, hop));
        };
        // 并发首用时别人可能已经建好了，复用它，免得各建一个池
        if let Some((pool, last_used)) = guard.get(&key) {
            last_used.store(now, Ordering::Relaxed);
            return Ok(pool.clone());
        }
        guard.retain(|_, (_, last_used)| {
            now.saturating_sub(last_used.load(Ordering::Relaxed)) < POOL_ENTRY_IDLE.as_secs()
        });
        let built = pool(&self.ssl, hop);
        guard.insert(key, (built.clone(), AtomicU64::new(now)));
        Ok(built)
    }

    /// 不分账号地发送（账号接口每次 `Connection: close`，连接不复用；测试也走这里）。
    pub(crate) async fn send(
        &self,
        request: reqwest::Request,
        proxy_url: Option<&str>,
    ) -> Result<reqwest::Response, UpstreamError> {
        self.send_for(None, request, proxy_url).await
    }

    /// 发送一个已构造好的 reqwest 请求，复用 `account` 自己的连接。`timeout()` 与 reqwest 同义：
    /// 总时限，覆盖到响应体读完。
    pub(crate) async fn send_for(
        &self,
        account: Option<&str>,
        request: reqwest::Request,
        proxy_url: Option<&str>,
    ) -> Result<reqwest::Response, UpstreamError> {
        let deadline = request
            .timeout()
            .map(|limit| tokio::time::Instant::now() + *limit);
        let forward_auth = match proxy_url.map(str::trim).filter(|s| !s.is_empty()) {
            Some(url) if request.url().scheme() == "http" => match Hop::parse(url)? {
                Hop::Http { auth, .. } => auth,
                _ => None,
            },
            _ => None,
        };
        let mut request: http::Request<reqwest::Body> = request
            .try_into()
            .map_err(|e: reqwest::Error| UpstreamError::Build(e.to_string()))?;
        // 明文目标经 HTTP 代理是转发（见 `Connector::connect`），认证头随请求走，同 reqwest
        if let Some(auth) = forward_auth.and_then(|a| http::HeaderValue::from_str(&a).ok()) {
            request
                .headers_mut()
                .entry(http::header::PROXY_AUTHORIZATION)
                .or_insert(auth);
        }
        if self.shape == Shape::Messages {
            arrange_messages(&mut request);
        }
        let pending = self.pool(account, proxy_url)?.request(request);
        let response = match deadline {
            Some(at) => tokio::time::timeout_at(at, pending)
                .await
                .map_err(|_| UpstreamError::Timeout)?,
            None => pending.await,
        }
        .map_err(|e| classify(&e))?;
        let (mut parts, body) = response.into_parts();
        let body = DeadlineBody {
            inner: body,
            deadline: deadline.map(|at| Box::pin(tokio::time::sleep_until(at))),
        };
        let body = decode(&mut parts.headers, body);
        Ok(reqwest::Response::from(http::Response::from_parts(
            parts, body,
        )))
    }
}

/// 补上真机这条连接上的 `Connection` / `Accept-Encoding` / `Host` / `Content-Length`，再按 [`MESSAGES_ORDER`]
/// 重排（同名多值保持原相对顺序）。`Host` 与 `Content-Length` 显式写进来，hyper 就不再追加到末尾。
fn arrange_messages(request: &mut http::Request<reqwest::Body>) {
    use http::header::{ACCEPT_ENCODING, CONNECTION, CONTENT_LENGTH, HOST};
    let authority = request
        .uri()
        .authority()
        .and_then(|a| http::HeaderValue::from_str(a.as_str()).ok());
    let length = http_body::Body::size_hint(request.body())
        .exact()
        .filter(|&n| n > 0);
    let headers = request.headers_mut();
    headers
        .entry(CONNECTION)
        .or_insert(http::HeaderValue::from_static(MESSAGES_CONNECTION));
    headers
        .entry(ACCEPT_ENCODING)
        .or_insert(http::HeaderValue::from_static(MESSAGES_ACCEPT_ENCODING));
    if let Some(authority) = authority {
        headers.entry(HOST).or_insert(authority);
    }
    if let Some(length) = length {
        headers.entry(CONTENT_LENGTH).or_insert(length.into());
    }
    let position = |name: &str| MESSAGES_ORDER.iter().position(|known| *known == name);
    // 已知头占偶数位，未知头共用 `x-client-request-id` 后面的奇数位；稳定排序保住未知头的插入顺序
    let unknown = position(MESSAGES_UNKNOWN_AFTER).map_or(usize::MAX, |at| at * 2 + 1);
    let mut names: Vec<http::HeaderName> = headers.keys().cloned().collect();
    names.sort_by_key(|name| position(name.as_str()).map_or(unknown, |at| at * 2));
    let mut old = std::mem::take(headers);
    for name in names {
        if let http::header::Entry::Occupied(entry) = old.entry(name.clone()) {
            for value in entry.remove_entry_mult().1 {
                headers.append(name.clone(), value);
            }
        }
    }
}

/// 按 `Content-Encoding` 流式解压响应体（真机声明了 gzip / deflate / br / zstd，上游就可能压缩，
/// SSE 也不例外）。解压后去掉 `Content-Encoding` / `Content-Length`，下游拿到的就是原文；
/// 认不出的编码原样交出。
fn decode(headers: &mut http::HeaderMap, body: DeadlineBody) -> reqwest::Body {
    use async_compression::tokio::bufread::{BrotliDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder};
    use tokio_util::io::{ReaderStream, StreamReader};
    let encoding = headers
        .get(http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase());
    let Some(encoding) =
        encoding.filter(|e| matches!(e.as_str(), "gzip" | "x-gzip" | "deflate" | "br" | "zstd"))
    else {
        return reqwest::Body::wrap(body);
    };
    headers.remove(http::header::CONTENT_ENCODING);
    headers.remove(http::header::CONTENT_LENGTH);
    let mut body = body;
    let chunks = futures::stream::poll_fn(move |cx| {
        loop {
            match std::task::ready!(http_body::Body::poll_frame(Pin::new(&mut body), cx)) {
                None => return Poll::Ready(None),
                Some(Err(error)) => {
                    // 总时限的 TimedOut 原样透出，下游照常按 `is_timeout()` 认
                    let error = match error.downcast::<std::io::Error>() {
                        Ok(io) => *io,
                        Err(other) => std::io::Error::other(other),
                    };
                    return Poll::Ready(Some(Err(error)));
                }
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        return Poll::Ready(Some(Ok(data)));
                    }
                }
            }
        }
    });
    let reader = StreamReader::new(chunks);
    match encoding.as_str() {
        "gzip" | "x-gzip" => {
            reqwest::Body::wrap_stream(ReaderStream::new(GzipDecoder::new(reader)))
        }
        "deflate" => reqwest::Body::wrap_stream(ReaderStream::new(ZlibDecoder::new(reader))),
        "br" => reqwest::Body::wrap_stream(ReaderStream::new(BrotliDecoder::new(reader))),
        _ => reqwest::Body::wrap_stream(ReaderStream::new(ZstdDecoder::new(reader))),
    }
}

fn ssl_connector(shape: Shape, extra_root: Option<&[u8]>) -> Result<SslConnector, UpstreamError> {
    let build = |e: boring::error::ErrorStack| UpstreamError::Build(format!("tls_profile: {e}"));
    let mut builder = SslConnector::builder(SslMethod::tls()).map_err(build)?;
    builder
        .set_min_proto_version(Some(SslVersion::TLS1_2))
        .map_err(build)?;
    builder
        .set_max_proto_version(Some(SslVersion::TLS1_3))
        .map_err(build)?;
    builder.set_curves_list(CURVES).map_err(build)?;
    match shape {
        Shape::Messages => {
            builder.set_cipher_list(CIPHERS).map_err(build)?;
            builder.set_alpn_protos(b"\x08http/1.1").map_err(build)?;
            builder.enable_ocsp_stapling();
            builder.enable_signed_cert_timestamps();
        }
        Shape::Account => builder.set_cipher_list(ACCOUNT_CIPHERS).map_err(build)?,
    }
    // 证书校验用 Mozilla 根（与 reqwest/rustls 的 webpki-roots 同一份），不依赖宿主机证书库
    let store = builder.cert_store_mut();
    for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        if let Ok(cert) = boring::x509::X509::from_der(der) {
            let _ = store.add_cert(cert);
        }
    }
    if let Some(der) = extra_root {
        store
            .add_cert(boring::x509::X509::from_der(der).map_err(build)?)
            .map_err(build)?;
    }
    Ok(builder.build())
}

fn pool(ssl: &SslConnector, hop: Hop) -> Pool {
    Client::builder(TokioExecutor::new())
        .pool_idle_timeout(POOL_IDLE)
        .build(Connector {
            ssl: ssl.clone(),
            hop: Arc::new(hop),
        })
}

/// 到目标前的那一跳。
enum Hop {
    Direct,
    /// HTTP CONNECT 隧道；`tls` = https 代理（先和代理握手）。
    Http {
        host: String,
        port: u16,
        tls: bool,
        auth: Option<String>,
    },
    /// SOCKS5；`remote_dns` = socks5h（目标域名交给代理解析）。
    Socks5 {
        host: String,
        port: u16,
        remote_dns: bool,
        auth: Option<(String, String)>,
    },
}

impl Hop {
    fn parse(raw: &str) -> Result<Self, UpstreamError> {
        let url = crate::http::parse_proxy_url(raw)
            .ok_or_else(|| UpstreamError::Build("proxy_url".to_owned()))?;
        let host = url
            .host_str()
            .ok_or_else(|| UpstreamError::Build("proxy_url".to_owned()))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        };
        let user = decode(url.username());
        let pass = url.password().map(decode).unwrap_or_default();
        let has_auth = !user.is_empty() || !pass.is_empty();
        Ok(match url.scheme() {
            "http" | "https" => Self::Http {
                host,
                port: url.port_or_known_default().unwrap_or(80),
                tls: url.scheme() == "https",
                auth: has_auth.then(|| {
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
                    )
                }),
            },
            scheme => Self::Socks5 {
                host,
                port: url.port().unwrap_or(1080),
                remote_dns: scheme == "socks5h",
                auth: has_auth.then_some((user, pass)),
            },
        })
    }

    /// 打通到 `target:port` 的字节流（尚未做目标 TLS）。`forward` = 明文目标经 HTTP 代理：
    /// 只连到代理、不建隧道，请求用绝对 URI 交给代理转发。
    async fn open(&self, target: &str, port: u16, forward: bool) -> Result<BoxIo, ConnectError> {
        match self {
            Self::Direct => TcpStream::connect((target, port))
                .await
                .map(|tcp| Box::new(nodelay(tcp)) as BoxIo)
                .map_err(|e| ConnectError::target(format!("tcp connect error: {e}"))),
            Self::Http {
                host,
                port: proxy_port,
                tls,
                auth,
            } => {
                let tcp = TcpStream::connect((host.as_str(), *proxy_port))
                    .await
                    .map(nodelay)
                    .map_err(|e| ConnectError::proxy(format!("tcp connect error: {e}")))?;
                let mut io: BoxIo = if *tls {
                    Box::new(proxy_tls(host, tcp).await?)
                } else {
                    Box::new(tcp)
                };
                if !forward {
                    http_connect(&mut io, target, port, auth.as_deref()).await?;
                }
                Ok(io)
            }
            Self::Socks5 {
                host,
                port: proxy_port,
                remote_dns,
                auth,
            } => {
                let mut tcp = TcpStream::connect((host.as_str(), *proxy_port))
                    .await
                    .map(nodelay)
                    .map_err(|e| ConnectError::proxy(format!("tcp connect error: {e}")))?;
                socks5_connect(&mut tcp, target, port, *remote_dns, auth.as_ref()).await?;
                Ok(Box::new(tcp))
            }
        }
    }

    fn is_proxy(&self) -> bool {
        !matches!(self, Self::Direct)
    }
}

/// 关掉 Nagle（同 reqwest 的缺省）：`HeadCase` 把头和体分两次写，开着 Nagle 第二次要等对端 ACK。
fn nodelay(tcp: TcpStream) -> TcpStream {
    let _ = tcp.set_nodelay(true);
    tcp
}

async fn proxy_tls(
    host: &str,
    tcp: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, ConnectError> {
    use tokio_rustls::rustls;
    let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| ConnectError::proxy(format!("proxy tls: {e}")))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| ConnectError::proxy(format!("proxy tls: {e}")))?;
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| ConnectError::proxy(format!("proxy tls: {e}")))
}

/// HTTP CONNECT。407 = 代理拒绝认证（算代理这一跳）；其余非 2xx 是代理报目标不可达，分不清归属。
async fn http_connect(
    io: &mut BoxIo,
    target: &str,
    port: u16,
    auth: Option<&str>,
) -> Result<(), ConnectError> {
    let authority = if target.contains(':') {
        format!("[{target}]:{port}")
    } else {
        format!("{target}:{port}")
    };
    let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = auth {
        head.push_str("Proxy-Authorization: ");
        head.push_str(auth);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    io.write_all(head.as_bytes())
        .await
        .map_err(|e| ConnectError::unknown(format!("proxy tunnel: {e}")))?;
    // 逐字节读到空行：CONNECT 成功后代理不会先发数据，多读一个字节就会吞掉目标的 TLS 记录
    let mut reply = Vec::with_capacity(128);
    while !reply.ends_with(b"\r\n\r\n") {
        if reply.len() >= MAX_PROXY_HEAD {
            return Err(ConnectError::unknown(
                "proxy tunnel: response head too large".into(),
            ));
        }
        let byte = io
            .read_u8()
            .await
            .map_err(|e| ConnectError::unknown(format!("proxy tunnel: {e}")))?;
        reply.push(byte);
    }
    let status = std::str::from_utf8(&reply)
        .ok()
        .and_then(|text| text.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    match status {
        200..=299 => Ok(()),
        407 => Err(ConnectError::proxy("proxy authorization required".into())),
        other => Err(ConnectError::unknown(format!(
            "unsuccessful tunnel: {other}"
        ))),
    }
}

/// SOCKS5（RFC 1928 / 1929）。用户名密码不被接受、不支持认证算代理这一跳；
/// 代理回报连不上目标（REP ≠ 0）分不清归属。
async fn socks5_connect(
    tcp: &mut TcpStream,
    target: &str,
    port: u16,
    remote_dns: bool,
    auth: Option<&(String, String)>,
) -> Result<(), ConnectError> {
    let io_err = |e: std::io::Error| ConnectError::unknown(format!("socks: {e}"));
    let greeting: &[u8] = if auth.is_some() {
        &[5, 2, 0, 2]
    } else {
        &[5, 1, 0]
    };
    tcp.write_all(greeting).await.map_err(io_err)?;
    let mut choice = [0u8; 2];
    tcp.read_exact(&mut choice).await.map_err(io_err)?;
    match (choice, auth) {
        ([5, 0], _) => {}
        ([5, 2], Some((user, pass))) => {
            let (user, pass) = (user.as_bytes(), pass.as_bytes());
            let too_long = |_| ConnectError::proxy("credentials not accepted".into());
            let mut request = vec![1, u8::try_from(user.len()).map_err(too_long)?];
            request.extend_from_slice(user);
            request.push(u8::try_from(pass.len()).map_err(too_long)?);
            request.extend_from_slice(pass);
            tcp.write_all(&request).await.map_err(io_err)?;
            let mut status = [0u8; 2];
            tcp.read_exact(&mut status).await.map_err(io_err)?;
            if status[1] != 0 {
                return Err(ConnectError::proxy("credentials not accepted".into()));
            }
        }
        ([5, 0xff], _) => {
            return Err(ConnectError::proxy(
                "server does not support user/pass authentication".into(),
            ));
        }
        _ => {
            return Err(ConnectError::proxy(
                "server implements authentication incorrectly".into(),
            ));
        }
    }
    let mut request = vec![5, 1, 0];
    if remote_dns {
        let name = target.as_bytes();
        request.push(3);
        request.push(
            u8::try_from(name.len())
                .map_err(|_| ConnectError::target("socks: host name too long".into()))?,
        );
        request.extend_from_slice(name);
    } else {
        let addr = tokio::net::lookup_host((target, port))
            .await
            .ok()
            .and_then(|mut addrs| addrs.next())
            .ok_or_else(|| ConnectError::target(format!("socks: cannot resolve {target}")))?;
        match addr.ip() {
            std::net::IpAddr::V4(ip) => {
                request.push(1);
                request.extend_from_slice(&ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                request.push(4);
                request.extend_from_slice(&ip.octets());
            }
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    tcp.write_all(&request).await.map_err(io_err)?;
    let mut reply = [0u8; 4];
    tcp.read_exact(&mut reply).await.map_err(io_err)?;
    if reply[1] != 0 {
        return Err(ConnectError::unknown(format!(
            "socks: connect failed ({})",
            reply[1]
        )));
    }
    let skip = match reply[3] {
        1 => 4,
        4 => 16,
        3 => usize::from(tcp.read_u8().await.map_err(io_err)?),
        _ => return Err(ConnectError::unknown("socks: bad reply".into())),
    };
    let mut rest = vec![0u8; skip + 2];
    tcp.read_exact(&mut rest).await.map_err(io_err)?;
    Ok(())
}

/// 连接阶段的失败，带上「是不是代理这一跳」的判定（语义同 `UpstreamError::proxy_hop_failed`）。
#[derive(Debug)]
pub(crate) struct ConnectError {
    proxy_hop: bool,
    timed_out: bool,
    detail: String,
}

impl ConnectError {
    fn proxy(detail: String) -> Self {
        Self {
            proxy_hop: true,
            timed_out: false,
            detail,
        }
    }
    fn target(detail: String) -> Self {
        Self {
            proxy_hop: false,
            timed_out: false,
            detail,
        }
    }
    fn unknown(detail: String) -> Self {
        Self::target(detail)
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ConnectError {}

fn classify(error: &hyper_util::client::legacy::Error) -> UpstreamError {
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(error);
    let mut found: Option<&ConnectError> = None;
    while let Some(err) = source {
        detail.push_str(": ");
        detail.push_str(&err.to_string());
        if let Some(connect) = err.downcast_ref::<ConnectError>() {
            found = Some(connect);
        }
        source = err.source();
    }
    match found {
        Some(connect) => UpstreamError::Unreachable {
            timed_out: connect.timed_out,
            proxy_hop: connect.proxy_hop,
            detail,
        },
        None if error.is_connect() => UpstreamError::Unreachable {
            timed_out: false,
            proxy_hop: false,
            detail,
        },
        None => UpstreamError::Stream(detail),
    }
}

#[derive(Clone)]
struct Connector {
    ssl: SslConnector,
    hop: Arc<Hop>,
}

impl tower_service::Service<Uri> for Connector {
    type Response = Conn;
    type Error = ConnectError;
    type Future = Pin<Box<dyn Future<Output = Result<Conn, ConnectError>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let this = self.clone();
        Box::pin(async move { this.connect(uri).await })
    }
}

impl Connector {
    async fn connect(&self, uri: Uri) -> Result<Conn, ConnectError> {
        let host = uri
            .host()
            .ok_or_else(|| ConnectError::target("missing host".into()))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let https = match uri.scheme_str() {
            Some("https") => true,
            // 明文 http 只出现在自建地址与测试 mock 上，没有握手可言，照常连通即可
            Some("http") => false,
            _ => {
                return Err(ConnectError::target(
                    "client_tls: unsupported scheme".into(),
                ));
            }
        };
        let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
        // 与 reqwest 一致：明文目标经 HTTP 代理走转发（不少代理只放行到 443 的 CONNECT）
        let forward = !https && matches!(*self.hop, Hop::Http { .. });
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, self.hop.open(&host, port, forward))
            .await
            .map_err(|_| ConnectError {
                proxy_hop: false,
                timed_out: true,
                detail: if self.hop.is_proxy() {
                    "proxy connect timed out".into()
                } else {
                    "connect timed out".into()
                },
            })??;
        if !https {
            return Ok(Conn::new(stream, forward));
        }
        let config = self
            .ssl
            .configure()
            .map_err(|e| ConnectError::target(format!("tls: {e}")))?;
        let tls = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_boring::connect(config, &host, stream),
        )
        .await
        .map_err(|_| ConnectError {
            proxy_hop: false,
            timed_out: true,
            detail: "tls handshake timed out".into(),
        })?
        .map_err(|e| ConnectError::target(format!("tls: {e}")))?;
        Ok(Conn::new(Box::new(tls), false))
    }
}

pub(crate) struct Conn {
    io: TokioIo<BoxIo>,
    case: HeadCase,
    /// 经 HTTP 代理转发：hyper 据此把请求行写成绝对 URI。
    forwarded: bool,
    /// 改好拼写、底层还没收下的请求头字节；下次写、flush 或关闭前先排空。
    pending: Vec<u8>,
}

impl Conn {
    fn new(io: BoxIo, forwarded: bool) -> Self {
        Self {
            io: TokioIo::new(io),
            forwarded,
            case: HeadCase::default(),
            pending: Vec::new(),
        }
    }

    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while !self.pending.is_empty() {
            let written = std::task::ready!(hyper::rt::Write::poll_write(
                Pin::new(&mut self.io),
                cx,
                &self.pending
            ))?;
            if written == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }
            self.pending.drain(..written);
        }
        Poll::Ready(Ok(()))
    }
}

impl Connection for Conn {
    fn connected(&self) -> Connected {
        Connected::new().proxy(self.forwarded)
    }
}

impl hyper::rt::Read for Conn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for Conn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_drain(cx))?;
        if let Some(limit) = this.case.passthrough(buf.len()) {
            let written = std::task::ready!(Pin::new(&mut this.io).poll_write(cx, &buf[..limit]))?;
            this.case.passed(written);
            return Poll::Ready(Ok(written));
        }
        let consumed = this.case.rewrite(buf, &mut this.pending);
        // 改好的头已归我们保管：底层暂时写不进也算收下（唤醒已登记），下次写或 flush 时排空
        if let Poll::Ready(Err(e)) = this.poll_drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(consumed))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}

/// 把 hyper 写出的小写头名改回真机拼写（[`WIRE_NAMES`]）：逐字节过请求行与头部，只换头名的
/// 大小写、长度不变；请求体按 Content-Length 原样放过。分块编码之类认不出请求边界的，
/// 此后整条连接原样放过（本路径的请求体都是定长的）。
#[derive(Default)]
struct HeadCase {
    wire: Wire,
    name: Vec<u8>,
    value: Vec<u8>,
    field: Field,
    length: u64,
    unframed: bool,
}

#[derive(Default)]
enum Wire {
    #[default]
    RequestLine,
    Name,
    Value,
    Body(u64),
    Raw,
}

#[derive(Default, PartialEq, Eq)]
enum Field {
    #[default]
    Other,
    ContentLength,
    TransferEncoding,
}

impl HeadCase {
    /// 正在放过请求体时，这次最多原样写多少字节。
    fn passthrough(&self, len: usize) -> Option<usize> {
        match self.wire {
            Wire::Body(left) => Some(usize::try_from(left).map_or(len, |left| left.min(len))),
            Wire::Raw => Some(len),
            _ => None,
        }
    }

    fn passed(&mut self, written: usize) {
        if let Wire::Body(left) = self.wire {
            let left = left.saturating_sub(u64::try_from(written).unwrap_or(u64::MAX));
            self.wire = if left == 0 {
                Wire::RequestLine
            } else {
                Wire::Body(left)
            };
        }
    }

    /// 改写头部字节进 `out`，返回吃下的字节数；遇到头部结束就停，后面的请求体交给 `passthrough`。
    fn rewrite(&mut self, buf: &[u8], out: &mut Vec<u8>) -> usize {
        for (at, &byte) in buf.iter().enumerate() {
            match self.wire {
                Wire::RequestLine => {
                    out.push(byte);
                    if byte == b'\n' {
                        self.wire = Wire::Name;
                    }
                }
                Wire::Name if byte == b':' => {
                    let lower = self.name.to_ascii_lowercase();
                    self.field = match lower.as_slice() {
                        b"content-length" => Field::ContentLength,
                        b"transfer-encoding" => Field::TransferEncoding,
                        _ => Field::Other,
                    };
                    let spelled = WIRE_NAMES
                        .iter()
                        .find(|wire| wire.as_bytes().eq_ignore_ascii_case(&lower))
                        .map_or(self.name.as_slice(), |wire| wire.as_bytes());
                    out.extend_from_slice(spelled);
                    out.push(byte);
                    self.name.clear();
                    self.value.clear();
                    self.wire = Wire::Value;
                }
                Wire::Name if byte == b'\n' && self.name.iter().all(|&b| b == b'\r') => {
                    out.append(&mut self.name);
                    out.push(byte);
                    self.wire = match (self.unframed, self.length) {
                        (true, _) => Wire::Raw,
                        (false, 0) => Wire::RequestLine,
                        (false, length) => Wire::Body(length),
                    };
                    self.length = 0;
                    return at + 1;
                }
                Wire::Name => self.name.push(byte),
                Wire::Value => {
                    out.push(byte);
                    if byte != b'\n' {
                        self.value.push(byte);
                        continue;
                    }
                    let value = String::from_utf8_lossy(&self.value);
                    match self.field {
                        Field::ContentLength => match value.trim().parse::<u64>() {
                            Ok(length) => self.length = length,
                            Err(_) => self.unframed = true,
                        },
                        Field::TransferEncoding => self.unframed = true,
                        Field::Other => {}
                    }
                    self.wire = Wire::Name;
                }
                Wire::Body(_) | Wire::Raw => return at,
            }
        }
        buf.len()
    }
}

/// 响应体也受总时限约束（reqwest `timeout()` 的语义）。超时报 `TimedOut`，
/// 下游按 reqwest 的 `is_timeout()` 照常识别。
struct DeadlineBody {
    inner: hyper::body::Incoming,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl http_body::Body for DeadlineBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        if let Some(sleep) = self.deadline.as_mut()
            && sleep.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(Some(Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "response body deadline elapsed",
            )))));
        }
        Pin::new(&mut self.inner)
            .poll_frame(cx)
            .map_err(|e| Box::new(e) as Self::Error)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
#[path = "client_tls_tests.rs"]
mod tests;
