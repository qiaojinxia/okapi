//! 趋势采样：worker 每分钟采一次服务器与中间件的关键数，追加到 Redis 列表
//! `ops:samples`（保留 24 小时 = 1440 点）。多副本时用 `SET NX EX` 租约，每分钟只有
//! 一个副本采；CPU 与网卡速率按本进程上一次读数算，副本轮换时间隔会拉长、仍按实际秒数折算。

use super::host::{self, Rates, Reading};
use super::probes::Probes;
use fred::interfaces::{KeysInterface, ListInterface};
use fred::types::{Expiration, SetOptions};
use serde_json::{Value, json};
use std::time::Duration;

pub const REDIS_KEY: &str = "ops:samples";
const LEASE_KEY: &str = "ops:samples:round";
const INTERVAL: Duration = Duration::from_mins(1);
pub const KEEP: i64 = 24 * 60;

pub async fn run(probes: Probes, node: String, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last: Option<Reading> = None;
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = tick.tick() => {}
        }
        if *stop.borrow() {
            return;
        }
        let claimed: Result<Option<String>, _> = probes
            .redis
            .set(
                LEASE_KEY,
                node.as_str(),
                Some(Expiration::EX(50)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        // 拿不到租约 = 别的副本这一分钟已经采了；Redis 不可达也就没处写，一并跳过
        if !matches!(claimed, Ok(Some(_))) {
            last = None;
            continue;
        }
        let now = host::reading();
        let rates = match &last {
            Some(prev) => Rates::between(prev, &now),
            // 刚启动没有上一次读数：现场隔 1 秒补一次，第一个点也有 CPU
            None => host::rates_now(Duration::from_secs(1)).await,
        };
        last = Some(now);
        let sample = Box::pin(collect(&probes, &node, rates)).await;
        if let Err(e) = append(&probes.redis, &sample).await {
            tracing::debug!(error = %e, "ops sample append failed");
        }
    }
}

async fn collect(probes: &Probes, node: &str, rates: Rates) -> Value {
    let h = host::host();
    let mid = Box::pin(probes.snapshot()).await;
    let pick = |section: &str, key: &str| mid[section].get(key).cloned().unwrap_or(Value::Null);
    json!({
        "t": chrono::Utc::now().timestamp(),
        "node": node,
        "cpu": rates.cpu_percent,
        "load1": h.load.map(|l| l[0]),
        "mem_used": h.memory.map(|m| m.total_bytes.saturating_sub(m.available_bytes)),
        "mem_total": h.memory.map(|m| m.total_bytes),
        "disk_used": h.disk.map(|d| d.total_bytes.saturating_sub(d.free_bytes)),
        "disk_total": h.disk.map(|d| d.total_bytes),
        "net_rx": rates.net_rx_bps,
        "net_tx": rates.net_tx_bps,
        "pg_conns": pick("postgres", "connections"),
        "pg_active": pick("postgres", "active"),
        "pg_bytes": pick("postgres", "database_bytes"),
        "redis_mem": pick("redis", "used_memory"),
        "redis_clients": pick("redis", "clients"),
        "redis_ops": pick("redis", "ops_per_sec"),
        "ch_mem": pick("clickhouse", "memory_resident"),
        "ch_bytes": pick("clickhouse", "database_bytes"),
        "ch_queries": pick("clickhouse", "queries"),
        "nats_bytes": mid["nats"]["jetstream"].get("storage_bytes").cloned().unwrap_or(Value::Null),
    })
}

async fn append(redis: &fred::clients::Client, sample: &Value) -> anyhow::Result<()> {
    let pipe = redis.pipeline();
    let _: () = pipe.rpush(REDIS_KEY, sample.to_string()).await?;
    let _: () = pipe.ltrim(REDIS_KEY, -KEEP, -1).await?;
    let _: Vec<fred::types::Value> = pipe.all().await?;
    Ok(())
}

/// 最近 `minutes` 分钟的样本（旧的在前）。
pub async fn load(redis: &fred::clients::Client, minutes: i64) -> anyhow::Result<Vec<Value>> {
    let minutes = minutes.clamp(1, KEEP);
    let raw: Vec<String> = redis.lrange(REDIS_KEY, -minutes, -1).await?;
    let since = chrono::Utc::now().timestamp() - minutes * 60;
    Ok(raw
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(r).ok())
        .filter(|v| v["t"].as_i64().is_some_and(|t| t >= since))
        .collect())
}
