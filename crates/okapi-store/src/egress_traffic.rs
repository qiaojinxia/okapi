//! 出口代理流量（ClickHouse `egress_traffic_minute`）：每个进程每分钟每个代理一行，读时求和，
//! 只保留 7 天。计数口径见 `okapi_providers::egress_meter`。
use crate::{ChClient, StoreError};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;

/// 一个进程一分钟内经某个代理的流量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinuteRow {
    /// 分钟起点（Unix 秒）。
    pub minute: i64,
    pub proxy_id: i64,
    pub node: String,
    pub up_bytes: u64,
    pub down_bytes: u64,
    pub requests: u64,
}

pub async fn insert(
    ch: &ChClient,
    rows: &[MinuteRow],
    dedup_token: &str,
) -> Result<(), StoreError> {
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "minute": chrono::DateTime::from_timestamp(row.minute, 0)
                    .unwrap_or_default()
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
                "proxy_id": row.proxy_id,
                "node": row.node,
                "up_bytes": row.up_bytes,
                "down_bytes": row.down_bytes,
                "requests": row.requests,
            })
        })
        .collect();
    ch.insert_json_each_row("egress_traffic_minute", &rows, dedup_token)
        .await
}

/// 页面可选的时间范围；粒度跟着范围走，点数都在 300 以内。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Hour,
    Day,
    Week,
}

impl Range {
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.unwrap_or("24h") {
            "1h" => Some(Self::Hour),
            "24h" => Some(Self::Day),
            "7d" => Some(Self::Week),
            _ => None,
        }
    }

    #[must_use]
    pub const fn span_secs(self) -> i64 {
        match self {
            Self::Hour => 3600,
            Self::Day => 86_400,
            Self::Week => 604_800,
        }
    }

    #[must_use]
    pub const fn bucket_secs(self) -> i64 {
        match self {
            Self::Hour => 60,
            Self::Day => 300,
            Self::Week => 3600,
        }
    }
}

/// 一个代理在一个时间桶里的流量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub proxy_id: i64,
    /// 桶起点（Unix 秒）。
    pub t: i64,
    pub up_bytes: u64,
    pub down_bytes: u64,
    pub requests: u64,
}

/// 范围内按桶聚合；`proxies` = None 不限代理，Some 只看这些（own 范围）。
pub async fn series(
    ch: &ChClient,
    range: Range,
    proxies: Option<&[i64]>,
) -> Result<Vec<Point>, StoreError> {
    let filter = match proxies {
        None => String::new(),
        Some([]) => return Ok(Vec::new()),
        Some(ids) => format!(
            " AND proxy_id IN ({})",
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        ),
    };
    let sql = format!(
        "SELECT proxy_id, \
                toUnixTimestamp(toStartOfInterval(minute, INTERVAL {bucket} SECOND)) AS t, \
                sum(up_bytes) AS up_bytes, sum(down_bytes) AS down_bytes, sum(requests) AS requests \
         FROM egress_traffic_minute \
         WHERE minute >= toStartOfInterval(now() - INTERVAL {span} SECOND, INTERVAL {bucket} SECOND){filter} \
         GROUP BY proxy_id, t ORDER BY proxy_id, t",
        bucket = range.bucket_secs(),
        span = range.span_secs(),
    );
    ch.query_json_each_row(&sql)
        .await?
        .iter()
        .map(|row| {
            Ok(Point {
                proxy_id: number(&row["proxy_id"])?
                    .try_into()
                    .map_err(|_| invalid())?,
                t: number(&row["t"])?.try_into().map_err(|_| invalid())?,
                up_bytes: number(&row["up_bytes"])?,
                down_bytes: number(&row["down_bytes"])?,
                requests: number(&row["requests"])?,
            })
        })
        .collect()
}

const fn invalid() -> StoreError {
    StoreError::InvalidData("egress traffic row")
}

/// CH 的 64 位整数在 JSON 里默认带引号。
fn number(value: &Value) -> Result<u64, StoreError> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(invalid)
}

/// 出口代理地址（解封后的完整 URL，去掉首尾空白）→ 代理 id。流量计数在进程里按 URL 记，
/// 落库前用它换成 id；同一地址录了两次时取 id 小的。
pub async fn proxy_ids_by_url(
    pool: &PgPool,
    master_key: Option<&str>,
) -> Result<HashMap<String, i64>, StoreError> {
    let rows: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id, url_ciphertext FROM proxies ORDER BY id DESC")
            .fetch_all(pool)
            .await?;
    let mut index = HashMap::with_capacity(rows.len());
    for (id, stored) in rows {
        // 解不开的（主密钥轮换中）跳过：它的流量这一轮记不上，不影响别的代理
        if let Ok(url) = crate::credential::open(master_key, &stored) {
            index.insert(url.trim().to_owned(), id);
        }
    }
    Ok(index)
}

/// own 范围能看到的代理 id。
pub async fn owned_proxy_ids(pool: &PgPool, owner: i64) -> Result<Vec<i64>, StoreError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM proxies WHERE owner_id = $1 ORDER BY id")
            .bind(owner)
            .fetch_all(pool)
            .await?,
    )
}
