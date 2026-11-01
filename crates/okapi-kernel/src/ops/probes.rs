//! 中间件占用：PostgreSQL / Redis / ClickHouse / NATS 各自的连接、内存、磁盘与吞吐。
//! 每项独立超时、独立出错：一个中间件挂了，面板照样显示其余几项，并把这一项标成不可达。

use fred::interfaces::{ClientLike, ServerInterface};
use futures::StreamExt as _;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::future::Future;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Probes {
    pub pg: PgPool,
    pub redis: fred::clients::Client,
    pub ch: Option<okapi_store::ChClient>,
    pub nats: Option<async_nats::Client>,
}

/// 超时或出错 → `{"ok": false, "error": "..."}`；成功的结果补上 `"ok": true`。
async fn guarded<F>(probe: F) -> Value
where
    F: Future<Output = anyhow::Result<Value>>,
{
    match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        Ok(Ok(mut v)) => {
            v["ok"] = json!(true);
            v
        }
        Ok(Err(e)) => json!({"ok": false, "error": e.to_string()}),
        Err(_) => json!({"ok": false, "error": "timeout"}),
    }
}

/// ClickHouse JSONEachRow 默认把 64 位整数写成字符串；两种都认。
fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.parse().ok())
}

/// `INFO` 文本 → 键值（跳过 `# Section` 与空行）。
#[must_use]
pub fn parse_redis_info(raw: &str) -> std::collections::HashMap<&str, &str> {
    raw.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.trim_end_matches('\r').split_once(':'))
        .collect()
}

/// `db0:keys=12,expires=3,avg_ttl=0` → (12, 3)，各库求和。
fn keyspace(info: &std::collections::HashMap<&str, &str>) -> (u64, u64) {
    info.iter()
        .filter(|(k, _)| k.starts_with("db") && k[2..].chars().all(|c| c.is_ascii_digit()))
        .fold((0, 0), |(keys, expires), (_, v)| {
            let field = |name: &str| {
                v.split(',')
                    .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            (keys + field("keys"), expires + field("expires"))
        })
}

impl Probes {
    pub async fn snapshot(&self) -> Value {
        let (postgres, redis, clickhouse, nats) = tokio::join!(
            guarded(self.postgres()),
            guarded(self.redis()),
            self.clickhouse_or_off(),
            self.nats_or_off(),
        );
        json!({"postgres": postgres, "redis": redis, "clickhouse": clickhouse, "nats": nats})
    }

    async fn clickhouse_or_off(&self) -> Value {
        if self.ch.is_none() {
            return json!({"ok": false, "configured": false});
        }
        guarded(self.clickhouse()).await
    }

    async fn nats_or_off(&self) -> Value {
        if self.nats.is_none() {
            return json!({"ok": false, "configured": false});
        }
        guarded(self.nats()).await
    }

    pub async fn postgres(&self) -> anyhow::Result<Value> {
        let row: (String, i32, i64, i64, i64, i64, Option<f64>, f64) = sqlx::query_as(
            "SELECT current_setting('server_version'),
                    current_setting('max_connections')::int,
                    (SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend'),
                    (SELECT count(*) FROM pg_stat_activity
                      WHERE backend_type = 'client backend' AND state = 'active'),
                    (SELECT count(*) FROM pg_stat_activity
                      WHERE backend_type = 'client backend' AND state LIKE 'idle in transaction%'),
                    pg_database_size(current_database()),
                    (SELECT extract(epoch FROM max(now() - query_start))::float8 FROM pg_stat_activity
                      WHERE state = 'active' AND pid <> pg_backend_pid()
                        AND backend_type = 'client backend'),
                    extract(epoch FROM now() - pg_postmaster_start_time())::float8",
        )
        .fetch_one(&self.pg)
        .await?;
        let (version, max_conn, conns, active, idle_tx, db_bytes, longest, uptime) = row;
        let stats: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT blks_hit, blks_read, xact_commit, xact_rollback, deadlocks
               FROM pg_stat_database WHERE datname = current_database()",
        )
        .fetch_one(&self.pg)
        .await?;
        let (hit, read, commit, rollback, deadlocks) = stats;
        let pool_max = self.pg.options().get_max_connections();
        Ok(json!({
            "version": version,
            "uptime_secs": uptime as i64,
            "connections": conns,
            "active": active,
            "idle_in_transaction": idle_tx,
            "max_connections": max_conn,
            "longest_query_secs": longest,
            "database_bytes": db_bytes,
            "cache_hit_ratio": (hit + read > 0).then(|| hit as f64 / (hit + read) as f64),
            "commits": commit,
            "rollbacks": rollback,
            "deadlocks": deadlocks,
            "pool": {"size": self.pg.size(), "idle": self.pg.num_idle(), "max": pool_max},
            "tables": self.largest_tables().await?,
        }))
    }

    /// 最大的几张表。分区表只数父表：父表自身不占空间（pg_total_relation_size 为 0），
    /// 把各分区的大小加回父表，否则日志表看着是 0。
    async fn largest_tables(&self) -> anyhow::Result<Value> {
        let mut tables: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT c.relname::text, pg_total_relation_size(c.oid), c.reltuples::bigint
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'm') AND NOT c.relispartition
              ORDER BY 2 DESC LIMIT 8",
        )
        .fetch_all(&self.pg)
        .await?;
        let parts: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT p.relname::text, sum(pg_total_relation_size(c.oid))::bigint,
                    sum(greatest(c.reltuples, 0))::bigint
               FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
               JOIN pg_class p ON p.oid = i.inhparent
               JOIN pg_namespace n ON n.oid = p.relnamespace
              WHERE n.nspname = 'public' GROUP BY p.relname",
        )
        .fetch_all(&self.pg)
        .await
        .unwrap_or_default();
        for (name, bytes, rows) in parts {
            match tables.iter_mut().find(|t| t.0 == name) {
                Some(t) => {
                    t.1 += bytes;
                    t.2 = t.2.max(0) + rows;
                }
                None => tables.push((name, bytes, rows)),
            }
        }
        tables.sort_by_key(|t| std::cmp::Reverse(t.1));
        tables.truncate(8);
        Ok(tables
            .into_iter()
            .map(|(name, bytes, rows)| json!({"name": name, "bytes": bytes, "rows": rows.max(0)}))
            .collect())
    }

    pub async fn redis(&self) -> anyhow::Result<Value> {
        let raw: String = self.redis.info(None).await?;
        let info = parse_redis_info(&raw);
        let get = |k: &str| info.get(k).and_then(|v| v.parse::<f64>().ok());
        let (keys, expires) = keyspace(&info);
        let hits = get("keyspace_hits").unwrap_or(0.0);
        let misses = get("keyspace_misses").unwrap_or(0.0);
        let dbsize: i64 = self.redis.dbsize().await.unwrap_or(-1);
        Ok(json!({
            "version": info.get("redis_version"),
            "uptime_secs": get("uptime_in_seconds"),
            "clients": get("connected_clients"),
            "blocked_clients": get("blocked_clients"),
            "used_memory": get("used_memory"),
            "used_memory_peak": get("used_memory_peak"),
            "used_memory_rss": get("used_memory_rss"),
            "maxmemory": get("maxmemory").filter(|m| *m > 0.0),
            "maxmemory_policy": info.get("maxmemory_policy"),
            "fragmentation_ratio": get("mem_fragmentation_ratio"),
            "ops_per_sec": get("instantaneous_ops_per_sec"),
            "hit_ratio": (hits + misses > 0.0).then(|| hits / (hits + misses)),
            "evicted_keys": get("evicted_keys"),
            "expired_keys": get("expired_keys"),
            "keys": keys,
            "keys_with_ttl": expires,
            "current_db_keys": dbsize,
            "rdb_last_bgsave_status": info.get("rdb_last_bgsave_status"),
            "aof_enabled": info.get("aof_enabled").map(|v| *v == "1"),
        }))
    }

    pub async fn clickhouse(&self) -> anyhow::Result<Value> {
        let ch = self
            .ch
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("clickhouse off"))?;
        let head = ch
            .query_json_each_row("SELECT version() AS v, uptime() AS up")
            .await?;
        let metrics = ch
            .query_json_each_row(
                "SELECT metric, value FROM system.metrics WHERE metric IN \
                 ('Query','TCPConnection','HTTPConnection','MemoryTracking','BackgroundMergesAndMutationsPoolTask')",
            )
            .await?;
        let resident = ch
            .query_json_each_row(
                "SELECT metric, value FROM system.asynchronous_metrics \
                 WHERE metric IN ('MemoryResident','OSMemoryTotal')",
            )
            .await
            .unwrap_or_default();
        let disks = ch
            .query_json_each_row("SELECT name, free_space, total_space FROM system.disks")
            .await?;
        let tables = ch
            .query_json_each_row(
                "SELECT table, sum(bytes_on_disk) AS bytes, sum(rows) AS rows, count() AS parts \
                 FROM system.parts WHERE active AND database = currentDatabase() \
                 GROUP BY table ORDER BY bytes DESC",
            )
            .await?;
        let metric = |rows: &[Value], name: &str| {
            rows.iter()
                .find(|r| r["metric"] == name)
                .and_then(|r| num(&r["value"]))
        };
        let total_bytes: i64 = tables.iter().filter_map(|t| int(&t["bytes"])).sum();
        let total_parts: i64 = tables.iter().filter_map(|t| int(&t["parts"])).sum();
        Ok(json!({
            "version": head.first().map(|r| r["v"].clone()),
            "uptime_secs": head.first().and_then(|r| int(&r["up"])),
            "queries": metric(&metrics, "Query"),
            "tcp_connections": metric(&metrics, "TCPConnection"),
            "http_connections": metric(&metrics, "HTTPConnection"),
            "memory_tracking": metric(&metrics, "MemoryTracking"),
            "background_merges": metric(&metrics, "BackgroundMergesAndMutationsPoolTask"),
            "memory_resident": metric(&resident, "MemoryResident"),
            "os_memory_total": metric(&resident, "OSMemoryTotal"),
            "disks": disks.iter().map(|d| json!({
                "name": d["name"], "free_bytes": int(&d["free_space"]), "total_bytes": int(&d["total_space"]),
            })).collect::<Vec<_>>(),
            "database_bytes": total_bytes,
            "parts": total_parts,
            "tables": tables.iter().take(8).map(|t| json!({
                "name": t["table"], "bytes": int(&t["bytes"]), "rows": int(&t["rows"]), "parts": int(&t["parts"]),
            })).collect::<Vec<_>>(),
        }))
    }

    pub async fn nats(&self) -> anyhow::Result<Value> {
        let client = self
            .nats
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("nats off"))?;
        let state = client.connection_state();
        let server = client.server_info();
        let counters = client.statistics();
        let js = async_nats::jetstream::new(client.clone());
        let jetstream = match js.query_account().await {
            Ok(a) => json!({
                "memory_bytes": a.memory, "storage_bytes": a.storage,
                "streams": a.streams, "consumers": a.consumers,
                "max_memory": a.limits.max_memory.filter(|m| *m > 0),
                "max_storage": a.limits.max_storage.filter(|m| *m > 0),
            }),
            Err(e) => json!({"error": e.to_string()}),
        };
        let mut streams = Vec::new();
        let mut list = js.streams();
        while let Some(Ok(info)) = list.next().await {
            streams.push(json!({
                "name": info.config.name,
                "messages": info.state.messages,
                "bytes": info.state.bytes,
                "consumers": info.state.consumer_count,
            }));
            if streams.len() >= 10 {
                break;
            }
        }
        Ok(json!({
            "state": state.to_string(),
            "version": server.version,
            "server_name": server.server_name,
            "max_payload": server.max_payload,
            "in_bytes": counters.in_bytes.load(Relaxed),
            "out_bytes": counters.out_bytes.load(Relaxed),
            "in_messages": counters.in_messages.load(Relaxed),
            "out_messages": counters.out_messages.load(Relaxed),
            "reconnects": counters.connects.load(Relaxed).saturating_sub(1),
            "jetstream": jetstream,
            "streams": streams,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_info_parses_sections_and_keyspace() {
        let raw = "# Server\r\nredis_version:7.2.4\r\n\r\n# Keyspace\r\ndb0:keys=12,expires=3,avg_ttl=0\r\ndb1:keys=1,expires=0,avg_ttl=0\r\n";
        let info = parse_redis_info(raw);
        assert_eq!(info.get("redis_version"), Some(&"7.2.4"));
        assert_eq!(keyspace(&info), (13, 3));
    }

    #[test]
    fn clickhouse_numbers_accept_quoted_64_bit() {
        assert_eq!(int(&json!("123")), Some(123));
        assert_eq!(num(&json!(1.5)), Some(1.5));
        assert_eq!(int(&json!(null)), None);
    }
}
