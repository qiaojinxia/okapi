//! 出口代理流量计数：进程内按代理 URL 累加，后台每分钟取走一次写进 ClickHouse。
//!
//! 两种口径：
//! - 订阅传输（`client_tls`）在到代理的连接上数字节，含 TLS 与请求头，接近代理商的计费口径；
//! - reqwest 发出的请求只数请求体与响应体（reqwest 不让包装底层连接），比实际流量偏小几个百分点。
//!
//! URL 含认证信息，只当内存里的键：不打日志、不落库，落库前由调用方换成代理 id。
use bytes::Bytes;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// 一个代理的累计量（自上次 [`drain`] 起）。
#[derive(Default)]
pub struct Counter {
    up: AtomicU64,
    down: AtomicU64,
    requests: AtomicU64,
}

impl Counter {
    fn add_up(&self, bytes: usize) {
        self.up.fetch_add(bytes as u64, Ordering::Relaxed);
    }
    fn add_down(&self, bytes: usize) {
        self.down.fetch_add(bytes as u64, Ordering::Relaxed);
    }
    fn add_request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn take(&self) -> Sample {
        Sample {
            up: self.up.swap(0, Ordering::Relaxed),
            down: self.down.swap(0, Ordering::Relaxed),
            requests: self.requests.swap(0, Ordering::Relaxed),
        }
    }
}

/// 一段时间内经某个代理的流量。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    /// 发往代理的字节（请求方向）。
    pub up: u64,
    /// 从代理收到的字节（响应方向）。
    pub down: u64,
    /// 请求数（订阅传输按请求计，reqwest 按发送计）。
    pub requests: u64,
}

impl Sample {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.up == 0 && self.down == 0 && self.requests == 0
    }
}

/// 代理 URL → 计数器。条目不摘除：连接池与在途响应还拿着 `Arc<Counter>`，摘掉后它们的
/// 计数就丢了；条目数以进程用过的代理地址为上限。
#[derive(Default)]
struct Meter {
    counters: RwLock<HashMap<String, Arc<Counter>>>,
}

impl Meter {
    fn counter(&self, proxy_url: Option<&str>) -> Option<Arc<Counter>> {
        let url = proxy_url.map(str::trim).filter(|s| !s.is_empty())?;
        if let Ok(guard) = self.counters.read()
            && let Some(hit) = guard.get(url)
        {
            return Some(hit.clone());
        }
        let mut guard = self.counters.write().ok()?;
        Some(guard.entry(url.to_owned()).or_default().clone())
    }

    fn drain(&self) -> Vec<(String, Sample)> {
        let Ok(guard) = self.counters.read() else {
            return Vec::new();
        };
        guard
            .iter()
            .map(|(url, counter)| (url.clone(), counter.take()))
            .filter(|(_, sample)| !sample.is_empty())
            .collect()
    }

    fn restore(&self, proxy_url: &str, sample: Sample) {
        if let Some(counter) = self.counter(Some(proxy_url)) {
            counter.up.fetch_add(sample.up, Ordering::Relaxed);
            counter.down.fetch_add(sample.down, Ordering::Relaxed);
            counter
                .requests
                .fetch_add(sample.requests, Ordering::Relaxed);
        }
    }
}

static METER: LazyLock<Meter> = LazyLock::new(Meter::default);

/// 该出口的计数器；直连（无代理）返回 None。
pub fn counter(proxy_url: Option<&str>) -> Option<Arc<Counter>> {
    METER.counter(proxy_url)
}

/// 取走全部代理的累计并清零，只返回有流量的。
pub fn drain() -> Vec<(String, Sample)> {
    METER.drain()
}

/// 写库失败时把取走的量放回去，下一轮一起写。
pub fn restore(proxy_url: &str, sample: Sample) {
    METER.restore(proxy_url, sample);
}

/// 记一次请求（订阅传输在发送处调用）。
pub(crate) fn count_request(counter: Option<&Counter>) {
    if let Some(counter) = counter {
        counter.add_request();
    }
}

/// 在到代理的连接上数字节。
pub(crate) struct MeteredIo<T> {
    inner: T,
    counter: Arc<Counter>,
}

impl<T> MeteredIo<T> {
    pub(crate) const fn new(inner: T, counter: Arc<Counter>) -> Self {
        Self { inner, counter }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for MeteredIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) {
            this.counter.add_down(buf.filled().len() - before);
        }
        polled
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for MeteredIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = polled {
            this.counter.add_up(written);
        }
        polled
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(written)) = polled {
            this.counter.add_up(written);
        }
        polled
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// 数响应体字节（reqwest 路径）。
struct MeteredBody {
    inner: reqwest::Body,
    counter: Arc<Counter>,
}

impl http_body::Body for MeteredBody {
    type Data = Bytes;
    type Error = reqwest::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &polled
            && let Some(data) = frame.data_ref()
        {
            this.counter.add_down(data.len());
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// 发出 reqwest 请求；经出口代理时把请求体与响应体字节计入该代理。错误照原样返回。
pub async fn send(
    request: reqwest::RequestBuilder,
    proxy_url: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let Some(counter) = counter(proxy_url) else {
        return request.send().await;
    };
    let (client, request) = request.build_split();
    let request = request?;
    // 流式请求体（multipart、转发的上传流）拿不到长度，按 0 计
    let up = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .map_or(0, <[u8]>::len);
    counter.add_request();
    counter.add_up(up);
    let response = client.execute(request).await?;
    let url = response.url().clone();
    let response: http::Response<reqwest::Body> = response.into();
    let (parts, inner) = response.into_parts();
    let body = reqwest::Body::wrap(MeteredBody { inner, counter });
    // 转成 http::Response 会丢掉响应 URL，重建时带回去
    let mut builder = http::Response::builder()
        .status(parts.status)
        .version(parts.version);
    if let Some(extensions) = builder.extensions_mut() {
        *extensions = parts.extensions;
    }
    if let Some(headers) = builder.headers_mut() {
        *headers = parts.headers;
    }
    let rebuilt = reqwest::ResponseBuilderExt::url(builder, url)
        .body(body)
        .unwrap_or_else(|_| http::Response::new(reqwest::Body::from(Bytes::new())));
    Ok(reqwest::Response::from(rebuilt))
}

/// `.send()` 的计量版：`.send_via(outbound.proxy_url.as_deref())`，调用链不用改。
pub(crate) trait MeteredSend {
    async fn send_via(self, proxy_url: Option<&str>) -> Result<reqwest::Response, reqwest::Error>;
}

impl MeteredSend for reqwest::RequestBuilder {
    async fn send_via(self, proxy_url: Option<&str>) -> Result<reqwest::Response, reqwest::Error> {
        send(self, proxy_url).await
    }
}

#[cfg(test)]
#[path = "egress_meter_tests.rs"]
mod tests;
