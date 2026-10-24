//! 出口代理流量落库：每分钟取走进程内的计数（`okapi_providers::egress_meter`），换成代理 id
//! 写进 ClickHouse。每个服务进程各写各的（行带 node），读时求和。
//!
//! 对不上代理 id 的地址（控制台测试还没保存的地址、已删除的代理）丢弃；写库失败就把量放回，
//! 下一轮一起写。
use crate::config::Config;
use okapi_providers::egress_meter::{self, Sample};
use okapi_store::egress_traffic::{self, MinuteRow};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const FLUSH_EVERY: Duration = Duration::from_mins(1);
/// 有对不上的地址时最多这么久重读一次代理表（改了地址、新建了代理）。
const INDEX_REFRESH: Duration = Duration::from_mins(1);

pub fn spawn(cfg: &Config) {
    let Some(url) = &cfg.clickhouse_url else {
        return;
    };
    let ch = match okapi_store::ChClient::new(url, "okapi") {
        Ok(ch) => ch,
        Err(e) => {
            tracing::warn!(error = %e, "egress traffic flusher disabled");
            return;
        }
    };
    // 独立的小连接池：一分钟读一次代理表，不占业务池
    let pg = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&cfg.database_url)
    {
        Ok(pg) => pg,
        Err(e) => {
            tracing::warn!(error = %e, "egress traffic flusher disabled");
            return;
        }
    };
    let mut flusher = Flusher::new(pg, ch, cfg.master_key.clone(), cfg.node.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_EVERY);
        tick.tick().await;
        loop {
            tick.tick().await;
            super::guarded("egress_traffic", flusher.flush()).await;
        }
    });
}

pub struct Flusher {
    pg: sqlx::PgPool,
    ch: okapi_store::ChClient,
    master_key: Option<String>,
    node: String,
    index: HashMap<String, i64>,
    loaded_at: Option<Instant>,
}

impl Flusher {
    #[must_use]
    pub fn new(
        pg: sqlx::PgPool,
        ch: okapi_store::ChClient,
        master_key: Option<String>,
        node: String,
    ) -> Self {
        Self {
            pg,
            ch,
            master_key,
            node,
            index: HashMap::new(),
            loaded_at: None,
        }
    }

    /// 取走本进程的计数写一轮（后台每分钟一次；测试直接调）。
    pub async fn flush(&mut self) {
        let drained = egress_meter::drain();
        if drained.is_empty() {
            return;
        }
        if drained.iter().any(|(url, _)| !self.index.contains_key(url))
            && self
                .loaded_at
                .is_none_or(|at| at.elapsed() >= INDEX_REFRESH)
        {
            match egress_traffic::proxy_ids_by_url(&self.pg, self.master_key.as_deref()).await {
                Ok(index) => {
                    self.index = index;
                    self.loaded_at = Some(Instant::now());
                }
                Err(e) => {
                    tracing::warn!(error = %e, "egress traffic: proxy index unavailable");
                    restore(&drained);
                    return;
                }
            }
        }
        let now = chrono::Utc::now().timestamp();
        let minute = now - now.rem_euclid(60);
        let known: Vec<(&str, Sample, i64)> = drained
            .iter()
            .filter_map(|(url, sample)| self.index.get(url).map(|id| (url.as_str(), *sample, *id)))
            .collect();
        let rows = rows(&known, minute, &self.node);
        let token = uuid::Uuid::new_v4().to_string();
        if let Err(e) = egress_traffic::insert(&self.ch, &rows, &token).await {
            tracing::warn!(error = %e, "egress traffic flush failed; retrying next minute");
            for (url, sample, _) in known {
                egress_meter::restore(url, sample);
            }
        }
    }
}

fn restore(drained: &[(String, Sample)]) {
    for (url, sample) in drained {
        egress_meter::restore(url, *sample);
    }
}

/// 每个代理一行（按 id 合并，防同一代理出现两个计数键）。
fn rows(known: &[(&str, Sample, i64)], minute: i64, node: &str) -> Vec<MinuteRow> {
    let mut by_proxy: HashMap<i64, Sample> = HashMap::new();
    for (_, sample, id) in known {
        let total = by_proxy.entry(*id).or_default();
        total.up += sample.up;
        total.down += sample.down;
        total.requests += sample.requests;
    }
    let mut rows: Vec<MinuteRow> = by_proxy
        .into_iter()
        .map(|(proxy_id, sample)| MinuteRow {
            minute,
            proxy_id,
            node: node.to_owned(),
            up_bytes: sample.up,
            down_bytes: sample.down,
            requests: sample.requests,
        })
        .collect();
    rows.sort_by_key(|row| row.proxy_id);
    rows
}
