//! 零外部依赖的 Prometheus 0.0.4 指标注册表与 HTTP 采集中间件。
//!
//! 设计约束：
//! - 热路径零堆分配、无全局大锁（固定分桶直方图与原子计数）；
//! - 标签基数硬封顶（超限归并到 `_other`），防止恶意路由或高基数模型名拖垮内存；
//! - `/metrics` 缺省仅放行环回与私网采集源（K8s Pod / Prometheus 内网直采开箱可用）；
//!   若配置 `OKAPI_METRICS_TOKEN`，则强制校验 Bearer 令牌（常量时间比较）。

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::IpAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};

pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

const HTTP_BUCKETS_US: &[u64] = &[
    5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_500_000, 5_000_000,
    10_000_000, 30_000_000, 60_000_000,
];

const TTFT_BUCKETS_US: &[u64] = &[
    50_000, 100_000, 250_000, 500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000, 30_000_000,
];

const BATCH_DURATION_BUCKETS_US: &[u64] = &[
    1_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 5_000_000,
];

const BATCH_SIZE_BUCKETS: &[u64] = &[1, 2, 4, 8, 16, 32, 64];

const MAX_HTTP_REQ_SERIES: usize = 512;
const MAX_HTTP_DUR_SERIES: usize = 256;
const MAX_UPSTREAM_SERIES: usize = 256;
const MAX_PROVIDER_SERIES: usize = 64;
const MAX_MAINT_SERIES: usize = 128;

/// 采集时刻从 `AppState` 实时读取的运行时水位快照（无后台轮询开销）。
#[derive(Debug, Clone, Default)]
pub struct RuntimeSnapshot {
    pub role: &'static str,
    pub pg_pool_size: u32,
    pub pg_pool_idle: usize,
    pub pricebook_epoch: i64,
    pub settle_backlog: usize,
    pub settle_in_flight: usize,
    pub settle_shedding: bool,
    pub cache_models: u64,
    pub cache_candidates: u64,
    pub cache_channel_cost: u64,
    pub cache_channel_control: u64,
    pub cache_settings: u64,
}

/// 固定分桶无锁直方图（微秒或无量纲整数计数）。
pub struct FixedHistogram {
    bounds: &'static [u64],
    buckets: Vec<AtomicU64>,
    count: AtomicU64,
    sum: AtomicU64,
    /// `true`：输入为微秒，导出按秒（`0.000000`）；`false`：按原整数导出。
    as_seconds: bool,
}

impl FixedHistogram {
    fn new(bounds: &'static [u64], as_seconds: bool) -> Self {
        let mut buckets = Vec::with_capacity(bounds.len());
        for _ in 0..bounds.len() {
            buckets.push(AtomicU64::new(0));
        }
        Self {
            bounds,
            buckets,
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            as_seconds,
        }
    }

    fn observe_raw(&self, value: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(value, Ordering::Relaxed);
        for (idx, &bound) in self.bounds.iter().enumerate() {
            if value <= bound {
                self.buckets[idx].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }

    fn observe_duration(&self, duration: Duration) {
        let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        self.observe_raw(micros);
    }

    fn write_series(&self, out: &mut String, name: &str, labels: &str) {
        let mut cumulative: u64 = 0;
        let sep = if labels.is_empty() { "" } else { "," };
        for (idx, &bound) in self.bounds.iter().enumerate() {
            cumulative = cumulative.saturating_add(self.buckets[idx].load(Ordering::Relaxed));
            let le = if self.as_seconds {
                format_micros_as_seconds(bound)
            } else {
                bound.to_string()
            };
            let _ = writeln!(
                out,
                "{name}_bucket{{{labels}{sep}le=\"{le}\"}} {cumulative}"
            );
        }
        let total = self.count.load(Ordering::Relaxed);
        let _ = writeln!(out, "{name}_bucket{{{labels}{sep}le=\"+Inf\"}} {total}");
        let sum_raw = self.sum.load(Ordering::Relaxed);
        let sum_text = if self.as_seconds {
            format_micros_as_seconds(sum_raw)
        } else {
            sum_raw.to_string()
        };
        if labels.is_empty() {
            let _ = writeln!(out, "{name}_sum {sum_text}");
            let _ = writeln!(out, "{name}_count {total}");
        } else {
            let _ = writeln!(out, "{name}_sum{{{labels}}} {sum_text}");
            let _ = writeln!(out, "{name}_count{{{labels}}} {total}");
        }
    }
}

struct BoundedCounterMap<K: Ord + Clone> {
    cap: usize,
    overflow_key: K,
    inner: RwLock<BTreeMap<K, Arc<AtomicU64>>>,
}

impl<K: Ord + Clone> BoundedCounterMap<K> {
    fn new(cap: usize, overflow_key: K) -> Self {
        Self {
            cap,
            overflow_key,
            inner: RwLock::new(BTreeMap::new()),
        }
    }

    fn inc(&self, key: K) {
        if let Ok(guard) = self.inner.read()
            && let Some(counter) = guard.get(&key)
        {
            counter.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let target = if guard.contains_key(&key) || guard.len() < self.cap {
            key
        } else {
            self.overflow_key.clone()
        };
        guard
            .entry(target)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Vec<(K, u64)> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }
}

struct BoundedHistogramMap<K: Ord + Clone> {
    cap: usize,
    overflow_key: K,
    bounds: &'static [u64],
    as_seconds: bool,
    inner: RwLock<BTreeMap<K, Arc<FixedHistogram>>>,
}

impl<K: Ord + Clone> BoundedHistogramMap<K> {
    fn new(cap: usize, overflow_key: K, bounds: &'static [u64], as_seconds: bool) -> Self {
        Self {
            cap,
            overflow_key,
            bounds,
            as_seconds,
            inner: RwLock::new(BTreeMap::new()),
        }
    }

    fn observe_duration(&self, key: K, duration: Duration) {
        if let Ok(guard) = self.inner.read()
            && let Some(hist) = guard.get(&key)
        {
            hist.observe_duration(duration);
            return;
        }
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let target = if guard.contains_key(&key) || guard.len() < self.cap {
            key
        } else {
            self.overflow_key.clone()
        };
        guard
            .entry(target)
            .or_insert_with(|| Arc::new(FixedHistogram::new(self.bounds, self.as_seconds)))
            .observe_duration(duration);
    }

    fn snapshot(&self) -> Vec<(K, Arc<FixedHistogram>)> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect()
    }
}

/// 进程级 Prometheus 指标注册表。
pub struct Metrics {
    http_requests: BoundedCounterMap<(String, String, String, &'static str)>,
    http_duration: BoundedHistogramMap<(String, String)>,
    http_in_flight_gateway: AtomicI64,
    http_in_flight_console: AtomicI64,
    upstream_requests: BoundedCounterMap<(String, &'static str)>,
    upstream_ttft: BoundedHistogramMap<String>,
    upstream_failovers: AtomicU64,
    auth_cache_hits: AtomicU64,
    auth_cache_misses: AtomicU64,
    routing_invalidations_local: AtomicU64,
    routing_invalidations_broadcast: AtomicU64,
    settlement_batches_ok: AtomicU64,
    settlement_batches_fallback: AtomicU64,
    settlement_batch_size: FixedHistogram,
    settlement_batch_duration: FixedHistogram,
    worker_maintenance_runs: BoundedCounterMap<(String, &'static str)>,
}

impl Metrics {
    fn new() -> Self {
        Self {
            http_requests: BoundedCounterMap::new(
                MAX_HTTP_REQ_SERIES,
                ("_other".into(), "_other".into(), "_other".into(), "other"),
            ),
            http_duration: BoundedHistogramMap::new(
                MAX_HTTP_DUR_SERIES,
                ("_other".into(), "_other".into()),
                HTTP_BUCKETS_US,
                true,
            ),
            http_in_flight_gateway: AtomicI64::new(0),
            http_in_flight_console: AtomicI64::new(0),
            upstream_requests: BoundedCounterMap::new(
                MAX_UPSTREAM_SERIES,
                ("_other".into(), "other"),
            ),
            upstream_ttft: BoundedHistogramMap::new(
                MAX_PROVIDER_SERIES,
                "_other".into(),
                TTFT_BUCKETS_US,
                true,
            ),
            upstream_failovers: AtomicU64::new(0),
            auth_cache_hits: AtomicU64::new(0),
            auth_cache_misses: AtomicU64::new(0),
            routing_invalidations_local: AtomicU64::new(0),
            routing_invalidations_broadcast: AtomicU64::new(0),
            settlement_batches_ok: AtomicU64::new(0),
            settlement_batches_fallback: AtomicU64::new(0),
            settlement_batch_size: FixedHistogram::new(BATCH_SIZE_BUCKETS, false),
            settlement_batch_duration: FixedHistogram::new(BATCH_DURATION_BUCKETS_US, true),
            worker_maintenance_runs: BoundedCounterMap::new(
                MAX_MAINT_SERIES,
                ("_other".into(), "other"),
            ),
        }
    }

    pub fn record_http(
        &self,
        role: &'static str,
        method: &str,
        route: &str,
        status: u16,
        duration: Duration,
    ) {
        let class = status_class(status);
        self.http_requests
            .inc((role.to_owned(), method.to_owned(), route.to_owned(), class));
        self.http_duration
            .observe_duration((role.to_owned(), route.to_owned()), duration);
    }

    fn in_flight_gauge(&self, role: &'static str) -> &AtomicI64 {
        if role == "console" {
            &self.http_in_flight_console
        } else {
            &self.http_in_flight_gateway
        }
    }

    pub fn record_upstream_outcome(&self, provider: &str, outcome: &'static str) {
        self.upstream_requests.inc((provider.to_owned(), outcome));
    }

    pub fn record_upstream_ttft(&self, provider: &str, duration: Duration) {
        self.upstream_ttft
            .observe_duration(provider.to_owned(), duration);
    }

    pub fn record_upstream_failover(&self) {
        self.upstream_failovers.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auth_cache(&self, hit: bool) {
        if hit {
            self.auth_cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.auth_cache_misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_routing_invalidation(&self, broadcast: bool) {
        if broadcast {
            self.routing_invalidations_broadcast
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.routing_invalidations_local
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_settlement_batch(&self, batch_size: usize, duration: Duration, fallbacks: usize) {
        if fallbacks < batch_size {
            self.settlement_batches_ok.fetch_add(1, Ordering::Relaxed);
        }
        if fallbacks > 0 {
            self.settlement_batches_fallback
                .fetch_add(1, Ordering::Relaxed);
        }
        self.settlement_batch_size
            .observe_raw(u64::try_from(batch_size).unwrap_or(u64::MAX));
        self.settlement_batch_duration.observe_duration(duration);
    }

    pub fn record_worker_maintenance(&self, job: &str, outcome: &'static str) {
        self.worker_maintenance_runs.inc((job.to_owned(), outcome));
    }

    /// 导出标准 Prometheus Text Exposition Format 0.0.4。
    #[must_use]
    pub fn render_prometheus(&self, snap: &RuntimeSnapshot) -> String {
        let mut out = String::with_capacity(4096);
        self.render_http_and_upstream(&mut out);
        self.render_caches_and_settlement(&mut out);
        Self::render_runtime_snapshot(&mut out, snap);
        out
    }

    fn render_http_and_upstream(&self, out: &mut String) {
        out.push_str("# HELP okapi_http_requests_total Total HTTP requests handled by role, method, route template and status class.\n");
        out.push_str("# TYPE okapi_http_requests_total counter\n");
        for ((role, method, route, class), count) in self.http_requests.snapshot() {
            let _ = writeln!(
                out,
                "okapi_http_requests_total{{role=\"{}\",method=\"{}\",route=\"{}\",status_class=\"{}\"}} {count}",
                esc(&role),
                esc(&method),
                esc(&route),
                esc(class)
            );
        }

        out.push_str(
            "# HELP okapi_http_request_duration_seconds HTTP request latency in seconds.\n",
        );
        out.push_str("# TYPE okapi_http_request_duration_seconds histogram\n");
        for ((role, route), hist) in self.http_duration.snapshot() {
            let labels = format!("role=\"{}\",route=\"{}\"", esc(&role), esc(&route));
            hist.write_series(out, "okapi_http_request_duration_seconds", &labels);
        }

        out.push_str("# HELP okapi_http_in_flight Current in-flight HTTP requests.\n");
        out.push_str("# TYPE okapi_http_in_flight gauge\n");
        let _ = writeln!(
            out,
            "okapi_http_in_flight{{role=\"gateway\"}} {}",
            self.http_in_flight_gateway.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "okapi_http_in_flight{{role=\"console\"}} {}",
            self.http_in_flight_console.load(Ordering::Relaxed)
        );

        out.push_str("# HELP okapi_upstream_requests_total Upstream provider outcomes.\n");
        out.push_str("# TYPE okapi_upstream_requests_total counter\n");
        for ((provider, outcome), count) in self.upstream_requests.snapshot() {
            let _ = writeln!(
                out,
                "okapi_upstream_requests_total{{provider=\"{}\",outcome=\"{}\"}} {count}",
                esc(&provider),
                esc(outcome)
            );
        }

        out.push_str(
            "# HELP okapi_upstream_ttft_seconds Time to first token from upstream providers.\n",
        );
        out.push_str("# TYPE okapi_upstream_ttft_seconds histogram\n");
        for (provider, hist) in self.upstream_ttft.snapshot() {
            let labels = format!("provider=\"{}\"", esc(&provider));
            hist.write_series(out, "okapi_upstream_ttft_seconds", &labels);
        }

        out.push_str(
            "# HELP okapi_upstream_failovers_total Cross-channel/key failover attempts.\n",
        );
        out.push_str("# TYPE okapi_upstream_failovers_total counter\n");
        let _ = writeln!(
            out,
            "okapi_upstream_failovers_total {}",
            self.upstream_failovers.load(Ordering::Relaxed)
        );
    }

    fn render_caches_and_settlement(&self, out: &mut String) {
        out.push_str("# HELP okapi_auth_cache_total Auth Redis cache hits and misses.\n");
        out.push_str("# TYPE okapi_auth_cache_total counter\n");
        let _ = writeln!(
            out,
            "okapi_auth_cache_total{{result=\"hit\"}} {}",
            self.auth_cache_hits.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "okapi_auth_cache_total{{result=\"miss\"}} {}",
            self.auth_cache_misses.load(Ordering::Relaxed)
        );

        out.push_str(
            "# HELP okapi_routing_cache_invalidations_total Routing cache invalidations.\n",
        );
        out.push_str("# TYPE okapi_routing_cache_invalidations_total counter\n");
        let _ = writeln!(
            out,
            "okapi_routing_cache_invalidations_total{{source=\"local\"}} {}",
            self.routing_invalidations_local.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "okapi_routing_cache_invalidations_total{{source=\"broadcast\"}} {}",
            self.routing_invalidations_broadcast.load(Ordering::Relaxed)
        );

        out.push_str("# HELP okapi_settlement_batches_total Micro-batch settlement executions.\n");
        out.push_str("# TYPE okapi_settlement_batches_total counter\n");
        let _ = writeln!(
            out,
            "okapi_settlement_batches_total{{result=\"ok\"}} {}",
            self.settlement_batches_ok.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "okapi_settlement_batches_total{{result=\"fallback\"}} {}",
            self.settlement_batches_fallback.load(Ordering::Relaxed)
        );

        out.push_str(
            "# HELP okapi_settlement_batch_size Number of settlements coalesced per micro-batch.\n",
        );
        out.push_str("# TYPE okapi_settlement_batch_size histogram\n");
        self.settlement_batch_size
            .write_series(out, "okapi_settlement_batch_size", "");

        out.push_str("# HELP okapi_settlement_batch_duration_seconds Settlement micro-batch commit latency.\n");
        out.push_str("# TYPE okapi_settlement_batch_duration_seconds histogram\n");
        self.settlement_batch_duration.write_series(
            out,
            "okapi_settlement_batch_duration_seconds",
            "",
        );

        let maint = self.worker_maintenance_runs.snapshot();
        if !maint.is_empty() {
            out.push_str("# HELP okapi_worker_maintenance_runs_total Periodic worker maintenance outcomes.\n");
            out.push_str("# TYPE okapi_worker_maintenance_runs_total counter\n");
            for ((job, outcome), count) in maint {
                let _ = writeln!(
                    out,
                    "okapi_worker_maintenance_runs_total{{job=\"{}\",outcome=\"{}\"}} {count}",
                    esc(&job),
                    esc(outcome)
                );
            }
        }
    }

    fn render_runtime_snapshot(out: &mut String, snap: &RuntimeSnapshot) {
        // 实时运行时水位（由被抓取角色的 AppState 现算）
        let role = esc(snap.role);
        let active = usize::try_from(snap.pg_pool_size)
            .unwrap_or(0)
            .saturating_sub(snap.pg_pool_idle);
        out.push_str("# HELP okapi_pg_pool_connections PostgreSQL pool connections by state.\n");
        out.push_str("# TYPE okapi_pg_pool_connections gauge\n");
        let _ = writeln!(
            out,
            "okapi_pg_pool_connections{{role=\"{role}\",state=\"total\"}} {}",
            snap.pg_pool_size
        );
        let _ = writeln!(
            out,
            "okapi_pg_pool_connections{{role=\"{role}\",state=\"idle\"}} {}",
            snap.pg_pool_idle
        );
        let _ = writeln!(
            out,
            "okapi_pg_pool_connections{{role=\"{role}\",state=\"active\"}} {active}"
        );

        out.push_str("# HELP okapi_pricebook_epoch Currently loaded PriceBook epoch.\n");
        out.push_str("# TYPE okapi_pricebook_epoch gauge\n");
        let _ = writeln!(out, "okapi_pricebook_epoch {}", snap.pricebook_epoch);

        out.push_str("# HELP okapi_settlement_backlog Pending background settlement writes.\n");
        out.push_str("# TYPE okapi_settlement_backlog gauge\n");
        let _ = writeln!(
            out,
            "okapi_settlement_backlog{{role=\"{role}\"}} {}",
            snap.settle_backlog
        );

        out.push_str("# HELP okapi_settlement_in_flight In-flight tracked settlement tasks.\n");
        out.push_str("# TYPE okapi_settlement_in_flight gauge\n");
        let _ = writeln!(
            out,
            "okapi_settlement_in_flight{{role=\"{role}\"}} {}",
            snap.settle_in_flight
        );

        out.push_str("# HELP okapi_settlement_shedding Whether admission overload shedding is active (1/0).\n");
        out.push_str("# TYPE okapi_settlement_shedding gauge\n");
        let _ = writeln!(
            out,
            "okapi_settlement_shedding{{role=\"{role}\"}} {}",
            u8::from(snap.settle_shedding)
        );

        out.push_str(
            "# HELP okapi_routing_cache_entries Approximate entries in local moka caches.\n",
        );
        out.push_str("# TYPE okapi_routing_cache_entries gauge\n");
        for (cache, count) in [
            ("models", snap.cache_models),
            ("candidates", snap.cache_candidates),
            ("channel_cost", snap.cache_channel_cost),
            ("channel_control", snap.cache_channel_control),
            ("settings", snap.cache_settings),
        ] {
            let _ = writeln!(
                out,
                "okapi_routing_cache_entries{{role=\"{role}\",cache=\"{cache}\"}} {count}"
            );
        }
    }
}

static GLOBAL: LazyLock<Metrics> = LazyLock::new(Metrics::new);

/// 全局指标注册表单例。
#[must_use]
pub fn global() -> &'static Metrics {
    &GLOBAL
}

struct InFlightGuard(&'static AtomicI64);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Axum 中间件：记录 HTTP 请求量、分桶耗时直方图与实时在途请求数。
pub async fn observe_http(role: &'static str, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("_unmatched", MatchedPath::as_str)
        .to_owned();
    if route == "/metrics" {
        return next.run(req).await;
    }
    let method = req.method().as_str().to_owned();
    let gauge = global().in_flight_gauge(role);
    gauge.fetch_add(1, Ordering::Relaxed);
    let _guard = InFlightGuard(gauge);
    let started = Instant::now();
    let resp = next.run(req).await;
    global().record_http(
        role,
        &method,
        &route,
        resp.status().as_u16(),
        started.elapsed(),
    );
    resp
}

/// `/metrics` 抓取鉴权：
/// - 配置了 `OKAPI_METRICS_TOKEN` 时强制校验 `Authorization: Bearer <token>`；
/// - 未配置时仅允许本机环回与私网地址（K8s Pod / 内网 Prometheus 直采），拒绝公网直访。
pub fn check_scrape_access(headers: &HeaderMap) -> Result<(), StatusCode> {
    let token = std::env::var("OKAPI_METRICS_TOKEN")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    check_scrape_access_with(headers, token.as_deref())
}

fn check_scrape_access_with(
    headers: &HeaderMap,
    expected_token: Option<&str>,
) -> Result<(), StatusCode> {
    if let Some(expected) = expected_token {
        let supplied = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map_or("", str::trim);
        return if constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
            Ok(())
        } else {
            Err(StatusCode::UNAUTHORIZED)
        };
    }
    // 未配 token：用经 `stamp_peer_ip` 核实过的 socket 对端地址判定内网/环回；
    // 无对端信息（进程内 oneshot 单测）放行。
    let Some(peer) = headers
        .get(super::clients::PEER_IP_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.parse::<IpAddr>().ok())
    else {
        return Ok(());
    };
    if is_internal_scrape_ip(peer) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn is_internal_scrape_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || (a == 100 && (64..=127).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal_scrape_ip(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

const fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

fn format_micros_as_seconds(micros: u64) -> String {
    let secs = micros / 1_000_000;
    let frac = micros % 1_000_000;
    if frac == 0 {
        return secs.to_string();
    }
    let frac_str = format!("{frac:06}");
    let trimmed = frac_str.trim_end_matches('0');
    format!("{secs}.{trimmed}")
}

fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn prometheus_exposition_renders_valid_monotonic_histograms_and_labels() {
        let m = Metrics::new();
        m.record_http(
            "gateway",
            "POST",
            "/v1/chat/completions",
            200,
            Duration::from_millis(120),
        );
        m.record_http(
            "gateway",
            "POST",
            "/v1/chat/completions",
            502,
            Duration::from_millis(40),
        );
        m.record_upstream_outcome("openai", "ok");
        m.record_upstream_ttft("openai", Duration::from_millis(310));
        m.record_upstream_failover();
        m.record_auth_cache(true);
        m.record_auth_cache(false);
        m.record_settlement_batch(8, Duration::from_millis(12), 0);
        m.record_worker_maintenance("reconcile", "ran");

        let body = m.render_prometheus(&RuntimeSnapshot {
            role: "gateway",
            pg_pool_size: 12,
            pg_pool_idle: 9,
            pricebook_epoch: 7,
            settle_backlog: 3,
            settle_in_flight: 2,
            settle_shedding: false,
            cache_models: 10,
            cache_candidates: 20,
            cache_channel_cost: 5,
            cache_channel_control: 4,
            cache_settings: 15,
        });
        assert!(body.contains("okapi_http_requests_total{role=\"gateway\",method=\"POST\",route=\"/v1/chat/completions\",status_class=\"2xx\"} 1"));
        assert!(body.contains("okapi_http_requests_total{role=\"gateway\",method=\"POST\",route=\"/v1/chat/completions\",status_class=\"5xx\"} 1"));
        assert!(body.contains("okapi_http_request_duration_seconds_bucket{role=\"gateway\",route=\"/v1/chat/completions\",le=\"0.05\"} 1"));
        assert!(body.contains("okapi_http_request_duration_seconds_bucket{role=\"gateway\",route=\"/v1/chat/completions\",le=\"0.25\"} 2"));
        assert!(body.contains("okapi_http_request_duration_seconds_sum{role=\"gateway\",route=\"/v1/chat/completions\"} 0.16"));
        assert!(body.contains("okapi_http_request_duration_seconds_count{role=\"gateway\",route=\"/v1/chat/completions\"} 2"));
        assert!(
            body.contains("okapi_upstream_ttft_seconds_bucket{provider=\"openai\",le=\"0.5\"} 1")
        );
        assert!(body.contains("okapi_pg_pool_connections{role=\"gateway\",state=\"active\"} 3"));
        assert!(body.contains("okapi_pricebook_epoch 7"));
        assert!(
            body.contains(
                "okapi_worker_maintenance_runs_total{job=\"reconcile\",outcome=\"ran\"} 1"
            )
        );
    }

    #[test]
    fn cardinality_cap_folds_excess_keys_into_overflow_bucket() {
        let map = BoundedCounterMap::new(2, "_other".to_owned());
        map.inc("a".into());
        map.inc("b".into());
        map.inc("c".into());
        map.inc("d".into());
        let snap: BTreeMap<_, _> = map.snapshot().into_iter().collect();
        assert_eq!(snap.get("a"), Some(&1));
        assert_eq!(snap.get("b"), Some(&1));
        assert_eq!(snap.get("_other"), Some(&2));
        assert!(!snap.contains_key("c"));
    }

    #[test]
    fn scrape_access_enforces_token_or_internal_ip() {
        let mut h = HeaderMap::new();
        h.insert(
            super::super::clients::PEER_IP_HEADER,
            HeaderValue::from_static("203.0.113.10"),
        );
        assert_eq!(
            check_scrape_access_with(&h, None),
            Err(StatusCode::FORBIDDEN)
        );

        h.insert(
            super::super::clients::PEER_IP_HEADER,
            HeaderValue::from_static("10.244.1.5"),
        );
        assert_eq!(check_scrape_access_with(&h, None), Ok(()));

        assert_eq!(
            check_scrape_access_with(&h, Some("secret-123")),
            Err(StatusCode::UNAUTHORIZED)
        );
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-123"),
        );
        assert_eq!(check_scrape_access_with(&h, Some("secret-123")), Ok(()));
    }
}
