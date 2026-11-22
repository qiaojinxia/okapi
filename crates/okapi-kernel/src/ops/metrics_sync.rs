//! 多节点运行时指标同步：每个运行进程每 10 秒向 Redis Hash `ops:metrics:nodes`
//! 写入本节点结构化快照与窗口速率；控制台聚合所有活跃节点，过期节点自动淘汰。

#![allow(clippy::cast_precision_loss)]

use crate::metrics::{self, HistogramData, NodeMetricsRates, NodeMetricsSnapshot, RuntimeSnapshot};
use fred::interfaces::{HashesInterface, KeysInterface};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const REDIS_KEY: &str = "ops:metrics:nodes";
const PUBLISH_INTERVAL: Duration = Duration::from_secs(10);
const STALE_SECS: i64 = 45;
const HASH_TTL_SECS: i64 = 120;

struct RateTracker {
    at: Instant,
    gw_req: u64,
    console_req: u64,
    gw_5xx: u64,
    failovers: u64,
    compactions: u64,
    gw_hist: HistogramData,
    ttft_hist: HistogramData,
}

impl RateTracker {
    fn capture() -> Self {
        let (gw_req, console_req, gw_5xx, failovers, compactions, gw_hist, ttft_hist) =
            metrics::global().rate_counters();
        Self {
            at: Instant::now(),
            gw_req,
            console_req,
            gw_5xx,
            failovers,
            compactions,
            gw_hist,
            ttft_hist,
        }
    }

    fn advance(&mut self) -> NodeMetricsRates {
        let next = Self::capture();
        let secs = next.at.duration_since(self.at).as_secs_f64();
        let rates = if secs >= 0.5 {
            let d_gw = next.gw_req.saturating_sub(self.gw_req);
            let d_console = next.console_req.saturating_sub(self.console_req);
            let d_5xx = next.gw_5xx.saturating_sub(self.gw_5xx);
            let d_fail = next.failovers.saturating_sub(self.failovers);
            let d_comp = next.compactions.saturating_sub(self.compactions);
            let d_gw_hist = next.gw_hist.delta_since(&self.gw_hist);
            let d_ttft_hist = next.ttft_hist.delta_since(&self.ttft_hist);
            NodeMetricsRates {
                gateway_rps: (d_gw as f64) / secs,
                console_rps: (d_console as f64) / secs,
                gateway_5xx_rate: (d_gw > 0).then(|| (d_5xx as f64) * 100.0 / (d_gw as f64)),
                failovers_per_min: (d_fail as f64) * 60.0 / secs,
                compactions_per_min: (d_comp as f64) * 60.0 / secs,
                recent_gw_p95_ms: d_gw_hist.summary_ms().p95,
                recent_ttft_p95_ms: d_ttft_hist.summary_ms().p95,
            }
        } else {
            NodeMetricsRates::default()
        };
        *self = next;
        rates
    }
}

/// 启动后台节点指标心跳发布器（每进程调用一次；`okapi all` 以 `role = "all"` 发布避免重复计数）。
pub fn spawn_publisher<F>(
    redis: fred::clients::Client,
    node: String,
    role: &'static str,
    runtime_fn: F,
) where
    F: Fn() -> RuntimeSnapshot + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let field = format!("{node}:{role}");
        let mut tracker = RateTracker::capture();
        let mut tick = tokio::time::interval(PUBLISH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let rates = tracker.advance();
            let runtime = runtime_fn();
            let snap = metrics::global().snapshot(&node, &runtime, rates);
            super::guarded("ops_metrics_publish", async {
                if let Err(e) = publish_snapshot(&redis, &field, &snap).await {
                    tracing::debug!(error = %e, node = %node, role, "ops metrics publish failed");
                }
            })
            .await;
        }
    });
}

/// 写入单节点指标快照到共享 Redis Hash（含过期续期）。
pub async fn publish_snapshot(
    redis: &fred::clients::Client,
    field: &str,
    snap: &NodeMetricsSnapshot,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(snap)?;
    let pipe = redis.pipeline();
    let _: () = pipe.hset(REDIS_KEY, (field.to_owned(), payload)).await?;
    let _: () = pipe.expire(REDIS_KEY, HASH_TTL_SECS, None).await?;
    let _: Vec<fred::types::Value> = pipe.all().await?;
    Ok(())
}

/// 读取所有活跃节点的指标快照，并顺手清理超时失联节点。
pub async fn load_live_nodes(
    redis: &fred::clients::Client,
) -> anyhow::Result<Vec<NodeMetricsSnapshot>> {
    let raw: HashMap<String, String> = redis.hgetall(REDIS_KEY).await?;
    let cutoff = chrono::Utc::now().timestamp() - STALE_SECS;
    let mut live = Vec::with_capacity(raw.len());
    let mut stale = Vec::new();
    for (field, payload) in raw {
        match serde_json::from_str::<NodeMetricsSnapshot>(&payload) {
            Ok(snap) if snap.ts >= cutoff => live.push(snap),
            _ => stale.push(field),
        }
    }
    if !stale.is_empty() {
        let _: Result<i64, _> = redis.hdel(REDIS_KEY, stale).await;
    }
    Ok(live)
}
