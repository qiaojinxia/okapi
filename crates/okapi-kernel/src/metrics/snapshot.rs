//! Prometheus 内存指标的结构化快照与多节点集群聚合（供运维监控页与 24h 趋势采样共用）。

#![allow(clippy::cast_precision_loss)]

use super::{Metrics, RuntimeSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

/// 可序列化、可跨节点精确合并的直方图桶数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistogramData {
    pub bounds: Vec<u64>,
    pub buckets: Vec<u64>,
    pub count: u64,
    pub sum: u64,
    pub as_seconds: bool,
}

/// 面向展示的分位数汇总（时间类单位统一毫秒，计数类保持原值）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HistogramSummary {
    pub count: u64,
    pub avg: Option<f64>,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
}

impl HistogramData {
    /// 合并另一节点的同构分桶直方图（与 Prometheus `sum by (le)` 数学等价）。
    pub fn merge(&mut self, other: &Self) {
        if other.count == 0 {
            return;
        }
        if self.bounds.is_empty() {
            self.bounds.clone_from(&other.bounds);
            self.buckets.clone_from(&other.buckets);
            self.count = other.count;
            self.sum = other.sum;
            self.as_seconds = other.as_seconds;
            return;
        }
        if self.bounds != other.bounds || self.buckets.len() != other.buckets.len() {
            return;
        }
        for (dst, &src) in self.buckets.iter_mut().zip(&other.buckets) {
            *dst = dst.saturating_add(src);
        }
        self.count = self.count.saturating_add(other.count);
        self.sum = self.sum.saturating_add(other.sum);
    }

    /// 计算与上一次采样之间的增量直方图（用于计算窗口内瞬时 P95，而非进程启动以来累计值）。
    #[must_use]
    pub fn delta_since(&self, prev: &Self) -> Self {
        if self.bounds != prev.bounds
            || self.buckets.len() != prev.buckets.len()
            || self.count < prev.count
        {
            return self.clone();
        }
        let buckets = self
            .buckets
            .iter()
            .zip(&prev.buckets)
            .map(|(&cur, &old)| cur.saturating_sub(old))
            .collect();
        Self {
            bounds: self.bounds.clone(),
            buckets,
            count: self.count.saturating_sub(prev.count),
            sum: self.sum.saturating_sub(prev.sum),
            as_seconds: self.as_seconds,
        }
    }

    /// 桶内线性插值估算分位数（对齐 Prometheus `histogram_quantile`）。
    #[must_use]
    pub fn quantile_raw(&self, q: f64) -> Option<f64> {
        if self.count == 0 || self.bounds.is_empty() || self.bounds.len() != self.buckets.len() {
            return None;
        }
        let target = q.clamp(0.0, 1.0) * (self.count as f64);
        let mut cumulative = 0u64;
        let mut lower = 0.0f64;
        for (&bound, &bucket_count) in self.bounds.iter().zip(&self.buckets) {
            let next_cum = cumulative.saturating_add(bucket_count);
            let upper = bound as f64;
            if (next_cum as f64) >= target {
                if bucket_count == 0 {
                    return Some(lower);
                }
                let frac = ((target - (cumulative as f64)) / (bucket_count as f64)).clamp(0.0, 1.0);
                return Some(lower + (upper - lower) * frac);
            }
            cumulative = next_cum;
            lower = upper;
        }
        // 落入 +Inf 桶时返回最后一个有限上界（与 Prometheus 一致）
        self.bounds.last().map(|&b| b as f64)
    }

    /// 微秒时间直方图汇总为毫秒（保留 1 位小数）。
    #[must_use]
    pub fn summary_ms(&self) -> HistogramSummary {
        if self.count == 0 {
            return HistogramSummary::default();
        }
        let to_ms = |us: f64| round1(us / 1_000.0);
        HistogramSummary {
            count: self.count,
            avg: Some(to_ms((self.sum as f64) / (self.count as f64))),
            p50: self.quantile_raw(0.50).map(to_ms),
            p95: self.quantile_raw(0.95).map(to_ms),
            p99: self.quantile_raw(0.99).map(to_ms),
        }
    }

    /// 无量纲直方图汇总（如微批大小）。
    #[must_use]
    pub fn summary_raw(&self) -> HistogramSummary {
        if self.count == 0 {
            return HistogramSummary::default();
        }
        HistogramSummary {
            count: self.count,
            avg: Some(round1((self.sum as f64) / (self.count as f64))),
            p50: self.quantile_raw(0.50).map(round1),
            p95: self.quantile_raw(0.95).map(round1),
            p99: self.quantile_raw(0.99).map(round1),
        }
    }
}

/// 节点最近采样周期的瞬时速率（由 `metrics_sync` 跨周期差分计算）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeMetricsRates {
    pub gateway_rps: f64,
    pub console_rps: f64,
    pub gateway_5xx_rate: Option<f64>,
    pub failovers_per_min: f64,
    pub compactions_per_min: f64,
    pub recent_gw_p95_ms: Option<f64>,
    pub recent_ttft_p95_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequestMetric {
    pub role: String,
    pub method: String,
    pub route: String,
    pub status_class: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpDurationMetric {
    pub role: String,
    pub route: String,
    pub hist: HistogramData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamRequestMetric {
    pub provider: String,
    pub outcome: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHistMetric {
    pub provider: String,
    pub hist: HistogramData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerMaintMetric {
    pub job: String,
    pub outcome: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolQualityMetric {
    pub pool: String,
    pub action: String,
    pub count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheEntriesJson {
    pub models: u64,
    pub candidates: u64,
    pub channel_cost: u64,
    pub channel_control: u64,
    pub pool_policies: u64,
    pub settings: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeMetricsJson {
    pub pg_pool_size: u32,
    pub pg_pool_idle: usize,
    pub pg_pool_active: usize,
    pub pricebook_epoch: i64,
    pub settle_backlog: usize,
    pub settle_in_flight: usize,
    pub settle_shedding: bool,
    pub caches: CacheEntriesJson,
}

/// 单节点发布到 Redis 的完整应用层指标快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetricsSnapshot {
    pub node: String,
    pub role: String,
    pub ts: i64,
    pub rates: NodeMetricsRates,
    pub http_in_flight_gateway: i64,
    pub http_in_flight_console: i64,
    pub http_requests: Vec<HttpRequestMetric>,
    pub http_duration: Vec<HttpDurationMetric>,
    pub upstream_requests: Vec<UpstreamRequestMetric>,
    pub upstream_ttft: Vec<ProviderHistMetric>,
    pub upstream_failovers: u64,
    pub auth_cache_hits: u64,
    pub auth_cache_misses: u64,
    pub routing_invalidations_local: u64,
    pub routing_invalidations_broadcast: u64,
    pub settlement_batches_ok: u64,
    pub settlement_batches_fallback: u64,
    pub settlement_batch_size: HistogramData,
    pub settlement_batch_duration: HistogramData,
    pub worker_maintenance_runs: Vec<WorkerMaintMetric>,
    pub pool_quality_events: Vec<PoolQualityMetric>,
    pub cache_compactions: u64,
    pub cache_compaction_saved_chars: u64,
    pub runtime: RuntimeMetricsJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMetricsSummary {
    pub gateway_rps: f64,
    pub console_rps: f64,
    pub gateway_in_flight: i64,
    pub console_in_flight: i64,
    pub gateway_requests_total: u64,
    pub gateway_5xx_total: u64,
    pub gateway_5xx_rate: Option<f64>,
    pub auth_cache_hits: u64,
    pub auth_cache_misses: u64,
    pub auth_cache_hit_ratio: Option<f64>,
    pub upstream_failovers: u64,
    pub failovers_per_min: f64,
    pub settle_backlog: usize,
    pub settle_in_flight: usize,
    pub settle_shedding: bool,
    pub settle_batches_ok: u64,
    pub settle_batches_fallback: u64,
    pub settle_batch_size: HistogramSummary,
    pub settle_batch_duration_ms: HistogramSummary,
    pub cache_compactions: u64,
    pub compactions_per_min: f64,
    pub cache_compaction_saved_chars: u64,
    pub pool_quality_events_total: u64,
    pub routing_invalidations_local: u64,
    pub routing_invalidations_broadcast: u64,
    pub gateway_latency_ms: HistogramSummary,
    pub upstream_ttft_ms: HistogramSummary,
    pub recent_gw_p95_ms: Option<f64>,
    pub recent_ttft_p95_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetricsRow {
    pub provider: String,
    pub total: u64,
    pub ok: u64,
    pub errors: u64,
    pub success_ratio: Option<f64>,
    pub outcomes: BTreeMap<String, u64>,
    pub ttft_ms: HistogramSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRouteMetricsRow {
    pub role: String,
    pub route: String,
    pub total: u64,
    pub s2xx: u64,
    pub s4xx: u64,
    pub s5xx: u64,
    pub latency_ms: HistogramSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerJobRow {
    pub job: String,
    pub total: u64,
    pub outcomes: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetricsSummary {
    pub node: String,
    pub role: String,
    pub ts: i64,
    pub gateway_rps: f64,
    pub console_rps: f64,
    pub in_flight: i64,
    pub runtime: RuntimeMetricsJson,
}

/// `GET /admin/monitor/metrics` 返回结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMetricsResponse {
    pub collected_at: String,
    pub source: String,
    pub node_count: usize,
    pub prometheus_path: &'static str,
    pub summary: ClusterMetricsSummary,
    pub providers: Vec<ProviderMetricsRow>,
    pub routes: Vec<HttpRouteMetricsRow>,
    pub pool_quality_events: Vec<PoolQualityMetric>,
    pub worker_jobs: Vec<WorkerJobRow>,
    pub nodes: Vec<NodeMetricsSummary>,
}

impl Metrics {
    fn snapshot_runtime(snap: &RuntimeSnapshot) -> RuntimeMetricsJson {
        let active = usize::try_from(snap.pg_pool_size)
            .unwrap_or(0)
            .saturating_sub(snap.pg_pool_idle);
        RuntimeMetricsJson {
            pg_pool_size: snap.pg_pool_size,
            pg_pool_idle: snap.pg_pool_idle,
            pg_pool_active: active,
            pricebook_epoch: snap.pricebook_epoch,
            settle_backlog: snap.settle_backlog,
            settle_in_flight: snap.settle_in_flight,
            settle_shedding: snap.settle_shedding,
            caches: CacheEntriesJson {
                models: snap.cache_models,
                candidates: snap.cache_candidates,
                channel_cost: snap.cache_channel_cost,
                channel_control: snap.cache_channel_control,
                pool_policies: snap.cache_pool_policies,
                settings: snap.cache_settings,
            },
        }
    }

    /// 采集本进程完整结构化快照（供 Redis 节点心跳与本进程降级读取）。
    #[must_use]
    pub fn snapshot(
        &self,
        node: &str,
        snap: &RuntimeSnapshot,
        rates: NodeMetricsRates,
    ) -> NodeMetricsSnapshot {
        NodeMetricsSnapshot {
            node: node.to_owned(),
            role: snap.role.to_owned(),
            ts: chrono::Utc::now().timestamp(),
            rates,
            http_in_flight_gateway: self.http_in_flight_gateway.load(Ordering::Relaxed),
            http_in_flight_console: self.http_in_flight_console.load(Ordering::Relaxed),
            http_requests: self
                .http_requests
                .snapshot()
                .into_iter()
                .map(|((role, method, route, class), count)| HttpRequestMetric {
                    role,
                    method,
                    route,
                    status_class: class.to_owned(),
                    count,
                })
                .collect(),
            http_duration: self
                .http_duration
                .snapshot()
                .into_iter()
                .map(|((role, route), hist)| HttpDurationMetric {
                    role,
                    route,
                    hist: hist.to_data(),
                })
                .collect(),
            upstream_requests: self
                .upstream_requests
                .snapshot()
                .into_iter()
                .map(|((provider, outcome), count)| UpstreamRequestMetric {
                    provider,
                    outcome: outcome.to_owned(),
                    count,
                })
                .collect(),
            upstream_ttft: self
                .upstream_ttft
                .snapshot()
                .into_iter()
                .map(|(provider, hist)| ProviderHistMetric {
                    provider,
                    hist: hist.to_data(),
                })
                .collect(),
            upstream_failovers: self.upstream_failovers.load(Ordering::Relaxed),
            auth_cache_hits: self.auth_cache_hits.load(Ordering::Relaxed),
            auth_cache_misses: self.auth_cache_misses.load(Ordering::Relaxed),
            routing_invalidations_local: self.routing_invalidations_local.load(Ordering::Relaxed),
            routing_invalidations_broadcast: self
                .routing_invalidations_broadcast
                .load(Ordering::Relaxed),
            settlement_batches_ok: self.settlement_batches_ok.load(Ordering::Relaxed),
            settlement_batches_fallback: self.settlement_batches_fallback.load(Ordering::Relaxed),
            settlement_batch_size: self.settlement_batch_size.to_data(),
            settlement_batch_duration: self.settlement_batch_duration.to_data(),
            worker_maintenance_runs: self
                .worker_maintenance_runs
                .snapshot()
                .into_iter()
                .map(|((job, outcome), count)| WorkerMaintMetric {
                    job,
                    outcome: outcome.to_owned(),
                    count,
                })
                .collect(),
            pool_quality_events: self
                .pool_quality_events
                .snapshot()
                .into_iter()
                .map(|((pool, action), count)| PoolQualityMetric {
                    pool,
                    action: action.to_owned(),
                    count,
                })
                .collect(),
            cache_compactions: self.cache_compactions.load(Ordering::Relaxed),
            cache_compaction_saved_chars: self.cache_compaction_saved_chars.load(Ordering::Relaxed),
            runtime: Self::snapshot_runtime(snap),
        }
    }

    /// 供 `metrics_sync` 跨周期计算瞬时速率与窗口 P95 的轻量读数。
    #[must_use]
    pub fn rate_counters(&self) -> (u64, u64, u64, u64, u64, HistogramData, HistogramData) {
        let mut gw_req = 0u64;
        let mut console_req = 0u64;
        let mut gw_5xx = 0u64;
        for ((role, _, _, class), count) in self.http_requests.snapshot() {
            if role == "gateway" {
                gw_req = gw_req.saturating_add(count);
                if class == "5xx" {
                    gw_5xx = gw_5xx.saturating_add(count);
                }
            } else if role == "console" {
                console_req = console_req.saturating_add(count);
            }
        }
        let mut gw_hist = HistogramData::default();
        for ((role, _), hist) in self.http_duration.snapshot() {
            if role == "gateway" {
                gw_hist.merge(&hist.to_data());
            }
        }
        let mut ttft_hist = HistogramData::default();
        for (_, hist) in self.upstream_ttft.snapshot() {
            ttft_hist.merge(&hist.to_data());
        }
        (
            gw_req,
            console_req,
            gw_5xx,
            self.upstream_failovers.load(Ordering::Relaxed),
            self.cache_compactions.load(Ordering::Relaxed),
            gw_hist,
            ttft_hist,
        )
    }
}

#[derive(Default)]
struct SeriesAcc {
    route_counts: BTreeMap<(String, String), (u64, u64, u64, u64)>,
    route_hists: BTreeMap<(String, String), HistogramData>,
    provider_outcomes: BTreeMap<String, BTreeMap<String, u64>>,
    provider_hists: BTreeMap<String, HistogramData>,
    quality_map: BTreeMap<(String, String), u64>,
    worker_map: BTreeMap<String, BTreeMap<String, u64>>,
    gw_hist: HistogramData,
    ttft_hist: HistogramData,
    batch_size_hist: HistogramData,
    batch_dur_hist: HistogramData,
}

impl SeriesAcc {
    fn absorb(&mut self, n: &NodeMetricsSnapshot, acc: &mut SummaryAcc) {
        self.batch_size_hist.merge(&n.settlement_batch_size);
        self.batch_dur_hist.merge(&n.settlement_batch_duration);
        for req in &n.http_requests {
            let entry = self
                .route_counts
                .entry((req.role.clone(), req.route.clone()))
                .or_default();
            entry.0 = entry.0.saturating_add(req.count);
            match req.status_class.as_str() {
                "2xx" => entry.1 = entry.1.saturating_add(req.count),
                "4xx" => entry.2 = entry.2.saturating_add(req.count),
                "5xx" => entry.3 = entry.3.saturating_add(req.count),
                _ => {}
            }
            if req.role == "gateway" {
                acc.gw_req_total = acc.gw_req_total.saturating_add(req.count);
                if req.status_class == "5xx" {
                    acc.gw_5xx_total = acc.gw_5xx_total.saturating_add(req.count);
                }
            }
        }
        for dur in &n.http_duration {
            self.route_hists
                .entry((dur.role.clone(), dur.route.clone()))
                .or_default()
                .merge(&dur.hist);
            if dur.role == "gateway" {
                self.gw_hist.merge(&dur.hist);
            }
        }
        for up in &n.upstream_requests {
            *self
                .provider_outcomes
                .entry(up.provider.clone())
                .or_default()
                .entry(up.outcome.clone())
                .or_insert(0) += up.count;
        }
        for ttft in &n.upstream_ttft {
            self.provider_hists
                .entry(ttft.provider.clone())
                .or_default()
                .merge(&ttft.hist);
            self.ttft_hist.merge(&ttft.hist);
        }
        for ev in &n.pool_quality_events {
            *self
                .quality_map
                .entry((ev.pool.clone(), ev.action.clone()))
                .or_insert(0) += ev.count;
            acc.pool_quality_total = acc.pool_quality_total.saturating_add(ev.count);
        }
        for w in &n.worker_maintenance_runs {
            *self
                .worker_map
                .entry(w.job.clone())
                .or_default()
                .entry(w.outcome.clone())
                .or_insert(0) += w.count;
        }
    }
}

/// 聚合多节点快照为集群视图（`source` = `"shared"` 或 `"local"`）。
#[must_use]
pub fn aggregate_cluster_metrics(
    nodes: &[NodeMetricsSnapshot],
    source: &'static str,
) -> ClusterMetricsResponse {
    let mut series = SeriesAcc::default();
    let mut acc = SummaryAcc::default();
    let mut node_rows = Vec::with_capacity(nodes.len());

    for n in nodes {
        acc.absorb_node(n);
        series.absorb(n, &mut acc);
        node_rows.push(NodeMetricsSummary {
            node: n.node.clone(),
            role: n.role.clone(),
            ts: n.ts,
            gateway_rps: round2(n.rates.gateway_rps),
            console_rps: round2(n.rates.console_rps),
            in_flight: n.http_in_flight_gateway + n.http_in_flight_console,
            runtime: n.runtime.clone(),
        });
    }
    node_rows.sort_by(|a, b| a.node.cmp(&b.node).then_with(|| a.role.cmp(&b.role)));

    ClusterMetricsResponse {
        collected_at: chrono::Utc::now().to_rfc3339(),
        source: source.to_owned(),
        node_count: nodes.len(),
        prometheus_path: "/metrics",
        summary: acc.into_summary(
            &series.gw_hist,
            &series.ttft_hist,
            &series.batch_size_hist,
            &series.batch_dur_hist,
        ),
        providers: build_provider_rows(&series.provider_outcomes, &series.provider_hists),
        routes: build_route_rows(series.route_counts, &series.route_hists),
        pool_quality_events: series
            .quality_map
            .into_iter()
            .map(|((pool, action), count)| PoolQualityMetric {
                pool,
                action,
                count,
            })
            .collect(),
        worker_jobs: series
            .worker_map
            .into_iter()
            .map(|(job, outcomes)| WorkerJobRow {
                job,
                total: outcomes.values().copied().sum(),
                outcomes,
            })
            .collect(),
        nodes: node_rows,
    }
}

#[derive(Default)]
struct SummaryAcc {
    gw_rps: f64,
    console_rps: f64,
    gw_in_flight: i64,
    console_in_flight: i64,
    gw_req_total: u64,
    gw_5xx_total: u64,
    auth_hits: u64,
    auth_misses: u64,
    failovers: u64,
    failovers_pm: f64,
    settle_backlog: usize,
    settle_in_flight: usize,
    settle_shedding: bool,
    settle_ok: u64,
    settle_fallback: u64,
    compactions: u64,
    compactions_pm: f64,
    compaction_saved: u64,
    pool_quality_total: u64,
    inv_local: u64,
    inv_broadcast: u64,
    recent_gw_p95_max: Option<f64>,
    recent_ttft_p95_max: Option<f64>,
    recent_5xx_weighted: f64,
    recent_5xx_weight: f64,
}

impl SummaryAcc {
    fn absorb_node(&mut self, n: &NodeMetricsSnapshot) {
        self.gw_rps += n.rates.gateway_rps;
        self.console_rps += n.rates.console_rps;
        self.gw_in_flight += n.http_in_flight_gateway;
        self.console_in_flight += n.http_in_flight_console;
        self.auth_hits = self.auth_hits.saturating_add(n.auth_cache_hits);
        self.auth_misses = self.auth_misses.saturating_add(n.auth_cache_misses);
        self.failovers = self.failovers.saturating_add(n.upstream_failovers);
        self.failovers_pm += n.rates.failovers_per_min;
        self.settle_backlog = self.settle_backlog.saturating_add(n.runtime.settle_backlog);
        self.settle_in_flight = self
            .settle_in_flight
            .saturating_add(n.runtime.settle_in_flight);
        self.settle_shedding |= n.runtime.settle_shedding;
        self.settle_ok = self.settle_ok.saturating_add(n.settlement_batches_ok);
        self.settle_fallback = self
            .settle_fallback
            .saturating_add(n.settlement_batches_fallback);
        self.compactions = self.compactions.saturating_add(n.cache_compactions);
        self.compactions_pm += n.rates.compactions_per_min;
        self.compaction_saved = self
            .compaction_saved
            .saturating_add(n.cache_compaction_saved_chars);
        self.inv_local = self.inv_local.saturating_add(n.routing_invalidations_local);
        self.inv_broadcast = self
            .inv_broadcast
            .saturating_add(n.routing_invalidations_broadcast);
        if let Some(p95) = n.rates.recent_gw_p95_ms {
            self.recent_gw_p95_max = Some(self.recent_gw_p95_max.map_or(p95, |cur| cur.max(p95)));
        }
        if let Some(p95) = n.rates.recent_ttft_p95_ms {
            self.recent_ttft_p95_max =
                Some(self.recent_ttft_p95_max.map_or(p95, |cur| cur.max(p95)));
        }
        if let Some(rate) = n.rates.gateway_5xx_rate {
            let weight = n.rates.gateway_rps.max(0.01);
            self.recent_5xx_weighted += rate * weight;
            self.recent_5xx_weight += weight;
        }
    }

    fn into_summary(
        self,
        gw_hist: &HistogramData,
        ttft_hist: &HistogramData,
        batch_size_hist: &HistogramData,
        batch_dur_hist: &HistogramData,
    ) -> ClusterMetricsSummary {
        let auth_total = self.auth_hits.saturating_add(self.auth_misses);
        let gw_5xx_rate = if self.recent_5xx_weight > 0.0 {
            Some(round2(self.recent_5xx_weighted / self.recent_5xx_weight))
        } else if self.gw_req_total > 0 {
            Some(round2(
                (self.gw_5xx_total as f64) * 100.0 / (self.gw_req_total as f64),
            ))
        } else {
            None
        };
        ClusterMetricsSummary {
            gateway_rps: round2(self.gw_rps),
            console_rps: round2(self.console_rps),
            gateway_in_flight: self.gw_in_flight,
            console_in_flight: self.console_in_flight,
            gateway_requests_total: self.gw_req_total,
            gateway_5xx_total: self.gw_5xx_total,
            gateway_5xx_rate: gw_5xx_rate,
            auth_cache_hits: self.auth_hits,
            auth_cache_misses: self.auth_misses,
            auth_cache_hit_ratio: (auth_total > 0)
                .then(|| round2((self.auth_hits as f64) * 100.0 / (auth_total as f64))),
            upstream_failovers: self.failovers,
            failovers_per_min: round2(self.failovers_pm),
            settle_backlog: self.settle_backlog,
            settle_in_flight: self.settle_in_flight,
            settle_shedding: self.settle_shedding,
            settle_batches_ok: self.settle_ok,
            settle_batches_fallback: self.settle_fallback,
            settle_batch_size: batch_size_hist.summary_raw(),
            settle_batch_duration_ms: batch_dur_hist.summary_ms(),
            cache_compactions: self.compactions,
            compactions_per_min: round2(self.compactions_pm),
            cache_compaction_saved_chars: self.compaction_saved,
            pool_quality_events_total: self.pool_quality_total,
            routing_invalidations_local: self.inv_local,
            routing_invalidations_broadcast: self.inv_broadcast,
            gateway_latency_ms: gw_hist.summary_ms(),
            upstream_ttft_ms: ttft_hist.summary_ms(),
            recent_gw_p95_ms: self.recent_gw_p95_max.map(round1),
            recent_ttft_p95_ms: self.recent_ttft_p95_max.map(round1),
        }
    }
}

fn build_provider_rows(
    outcomes_map: &BTreeMap<String, BTreeMap<String, u64>>,
    hists: &BTreeMap<String, HistogramData>,
) -> Vec<ProviderMetricsRow> {
    let mut keys: Vec<String> = outcomes_map
        .keys()
        .chain(hists.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut rows: Vec<ProviderMetricsRow> = keys
        .drain(..)
        .map(|provider| {
            let outcomes = outcomes_map.get(&provider).cloned().unwrap_or_default();
            let total: u64 = outcomes.values().copied().sum();
            let ok = outcomes.get("ok").copied().unwrap_or(0);
            let errors = total.saturating_sub(ok);
            let ttft_ms = hists
                .get(&provider)
                .map(HistogramData::summary_ms)
                .unwrap_or_default();
            ProviderMetricsRow {
                provider,
                total,
                ok,
                errors,
                success_ratio: (total > 0).then(|| round2((ok as f64) * 100.0 / (total as f64))),
                outcomes,
                ttft_ms,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then_with(|| a.provider.cmp(&b.provider))
    });
    rows
}

fn build_route_rows(
    counts: BTreeMap<(String, String), (u64, u64, u64, u64)>,
    hists: &BTreeMap<(String, String), HistogramData>,
) -> Vec<HttpRouteMetricsRow> {
    let mut rows: Vec<HttpRouteMetricsRow> = counts
        .into_iter()
        .map(|((role, route), (total, s2xx, s4xx, s5xx))| {
            let latency_ms = hists
                .get(&(role.clone(), route.clone()))
                .map(HistogramData::summary_ms)
                .unwrap_or_default();
            HttpRouteMetricsRow {
                role,
                route,
                total,
                s2xx,
                s4xx,
                s5xx,
                latency_ms,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then_with(|| a.role.cmp(&b.role))
            .then_with(|| a.route.cmp(&b.route))
    });
    rows.truncate(30);
    rows
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn histogram_quantile_merge_and_delta_behave_consistently() {
        let a = HistogramData {
            bounds: vec![10_000, 50_000, 100_000, 500_000],
            buckets: vec![2, 6, 2, 0],
            count: 10,
            sum: 350_000,
            as_seconds: true,
        };
        let b = HistogramData {
            bounds: vec![10_000, 50_000, 100_000, 500_000],
            buckets: vec![0, 4, 6, 0],
            count: 10,
            sum: 650_000,
            as_seconds: true,
        };
        let mut merged = a.clone();
        merged.merge(&b);
        assert_eq!(merged.count, 20);
        assert_eq!(merged.buckets, vec![2, 10, 8, 0]);
        let s = merged.summary_ms();
        assert_eq!(s.avg, Some(50.0));
        assert!(s.p50.is_some_and(|v| (30.0..=50.0).contains(&v)));
        assert!(s.p95.is_some_and(|v| (80.0..=100.0).contains(&v)));

        let delta = merged.delta_since(&a);
        assert_eq!(delta.count, 10);
        assert_eq!(delta.buckets, b.buckets);
    }

    #[test]
    fn aggregate_cluster_metrics_sums_nodes_and_phase2_counters() {
        let m = Metrics::new();
        m.record_http(
            "gateway",
            "POST",
            "/v1/chat/completions",
            200,
            Duration::from_millis(80),
        );
        m.record_http(
            "gateway",
            "POST",
            "/v1/chat/completions",
            502,
            Duration::from_millis(200),
        );
        m.record_upstream_outcome("anthropic", "ok");
        m.record_upstream_outcome("anthropic", "http_5xx");
        m.record_upstream_ttft("anthropic", Duration::from_millis(420));
        m.record_upstream_failover();
        m.record_auth_cache(true);
        m.record_auth_cache(false);
        m.record_pool_quality_event("default", "demote_error_rate");
        m.record_cache_compaction(12_000);

        let rt = RuntimeSnapshot {
            role: "gateway",
            pg_pool_size: 10,
            pg_pool_idle: 6,
            pricebook_epoch: 5,
            settle_backlog: 4,
            settle_in_flight: 1,
            settle_shedding: false,
            cache_models: 8,
            cache_candidates: 12,
            cache_channel_cost: 3,
            cache_channel_control: 2,
            cache_pool_policies: 4,
            cache_settings: 9,
        };
        let n1 = m.snapshot(
            "gw-1",
            &rt,
            NodeMetricsRates {
                gateway_rps: 2.5,
                failovers_per_min: 3.0,
                compactions_per_min: 1.5,
                recent_gw_p95_ms: Some(180.0),
                recent_ttft_p95_ms: Some(450.0),
                ..Default::default()
            },
        );
        let n2 = m.snapshot(
            "gw-2",
            &rt,
            NodeMetricsRates {
                gateway_rps: 1.5,
                failovers_per_min: 1.0,
                compactions_per_min: 0.5,
                recent_gw_p95_ms: Some(220.0),
                recent_ttft_p95_ms: Some(400.0),
                ..Default::default()
            },
        );
        let cluster = aggregate_cluster_metrics(&[n1, n2], "shared");
        assert_eq!(cluster.node_count, 2);
        assert_eq!(cluster.summary.gateway_rps, 4.0);
        assert_eq!(cluster.summary.gateway_requests_total, 4);
        assert_eq!(cluster.summary.gateway_5xx_total, 2);
        assert_eq!(cluster.summary.gateway_5xx_rate, Some(50.0));
        assert_eq!(cluster.summary.auth_cache_hit_ratio, Some(50.0));
        assert_eq!(cluster.summary.settle_backlog, 8);
        assert_eq!(cluster.summary.pool_quality_events_total, 2);
        assert_eq!(cluster.summary.cache_compactions, 2);
        assert_eq!(cluster.summary.cache_compaction_saved_chars, 24_000);
        assert_eq!(cluster.summary.recent_gw_p95_ms, Some(220.0));
        assert_eq!(cluster.summary.recent_ttft_p95_ms, Some(450.0));
        assert_eq!(cluster.providers.len(), 1);
        assert_eq!(cluster.providers[0].provider, "anthropic");
        assert_eq!(cluster.providers[0].success_ratio, Some(50.0));
        assert_eq!(cluster.nodes[0].runtime.caches.pool_policies, 4);
    }
}
