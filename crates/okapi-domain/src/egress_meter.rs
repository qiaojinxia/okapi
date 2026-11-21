//! 进程内出口代理流量计数器（按代理 URL 累加，后台每分钟取走一次写进 ClickHouse）。
//!
//! 放在 `okapi-domain`（纯标准库、零外部依赖），使 `okapi-providers`（写入侧）与
//! `okapi-kernel`（后台刷新侧）共享同一份进程内计数器而无需相互依赖。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

/// 一个代理的累计量（自上次 [`drain`] 起）。
#[derive(Default)]
pub struct Counter {
    up: AtomicU64,
    down: AtomicU64,
    requests: AtomicU64,
}

impl Counter {
    pub fn add_up(&self, bytes: usize) {
        self.up.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn add_down(&self, bytes: usize) {
        self.down.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn add_request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn take(&self) -> Sample {
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
pub struct Meter {
    counters: RwLock<HashMap<String, Arc<Counter>>>,
}

impl Meter {
    #[must_use]
    pub fn counter(&self, proxy_url: Option<&str>) -> Option<Arc<Counter>> {
        let url = proxy_url.map(str::trim).filter(|s| !s.is_empty())?;
        if let Ok(guard) = self.counters.read()
            && let Some(hit) = guard.get(url)
        {
            return Some(hit.clone());
        }
        let mut guard = self.counters.write().ok()?;
        Some(guard.entry(url.to_owned()).or_default().clone())
    }

    #[must_use]
    pub fn drain(&self) -> Vec<(String, Sample)> {
        let Ok(guard) = self.counters.read() else {
            return Vec::new();
        };
        guard
            .iter()
            .map(|(url, counter)| (url.clone(), counter.take()))
            .filter(|(_, sample)| !sample.is_empty())
            .collect()
    }

    pub fn restore(&self, proxy_url: &str, sample: Sample) {
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
#[must_use]
pub fn counter(proxy_url: Option<&str>) -> Option<Arc<Counter>> {
    METER.counter(proxy_url)
}

/// 取走全部代理的累计并清零，只返回有流量的。
#[must_use]
pub fn drain() -> Vec<(String, Sample)> {
    METER.drain()
}

/// 写库失败时把取走的量放回去，下一轮一起写。
pub fn restore(proxy_url: &str, sample: Sample) {
    METER.restore(proxy_url, sample);
}
