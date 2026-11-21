use super::{BoxIo, CONNECT_TIMEOUT, Hop, WIRE_NAMES};
use crate::UpstreamError;
use boring::ssl::SslConnector;
use bytes::Bytes;
use http::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// 连接阶段的失败，带上「是不是代理这一跳」的判定（语义同 `UpstreamError::proxy_hop_failed`）。
#[derive(Debug)]
pub(crate) struct ConnectError {
    proxy_hop: bool,
    timed_out: bool,
    detail: String,
}

impl ConnectError {
    pub(super) fn proxy(detail: String) -> Self {
        Self {
            proxy_hop: true,
            timed_out: false,
            detail,
        }
    }
    pub(super) fn target(detail: String) -> Self {
        Self {
            proxy_hop: false,
            timed_out: false,
            detail,
        }
    }
    pub(super) fn unknown(detail: String) -> Self {
        Self::target(detail)
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ConnectError {}

pub(super) fn classify(error: &hyper_util::client::legacy::Error) -> UpstreamError {
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
pub(super) struct Connector {
    pub(super) ssl: SslConnector,
    pub(super) hop: Arc<Hop>,
    /// 经代理时数这条连接上的字节（含目标 TLS 与请求头）。
    pub(super) meter: Option<Arc<crate::egress_meter::Counter>>,
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
        let stream: BoxIo = match &self.meter {
            Some(counter) => Box::new(crate::egress_meter::MeteredIo::new(stream, counter.clone())),
            None => stream,
        };
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
pub(super) struct HeadCase {
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
    pub(super) fn passthrough(&self, len: usize) -> Option<usize> {
        match self.wire {
            Wire::Body(left) => Some(usize::try_from(left).map_or(len, |left| left.min(len))),
            Wire::Raw => Some(len),
            _ => None,
        }
    }

    pub(super) fn passed(&mut self, written: usize) {
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
    pub(super) fn rewrite(&mut self, buf: &[u8], out: &mut Vec<u8>) -> usize {
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
pub(super) struct DeadlineBody {
    pub(super) inner: hyper::body::Incoming,
    pub(super) deadline: Option<Pin<Box<tokio::time::Sleep>>>,
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
