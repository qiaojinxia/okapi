//! ClickHouse 薄客户端：HTTP 接口 + JSONEachRow。
//!
//! 取舍：官方 clickhouse crate（RowBinary）留作 M3 性能优化项；HTTP + JSONEachRow
//! 实现简单、可观察，且原生支持 `insert_deduplication_token`（docs/database.md §3.3
//! 批次幂等）。查询统一带护栏：max_execution_time=15s、max_memory_usage=2GiB。

use crate::error::StoreError;
use std::time::Duration;

const QUERY_GUARD: &str = "max_execution_time=15&max_memory_usage=2000000000";

#[derive(Clone)]
pub struct ChClient {
    http: reqwest::Client,
    base: String,
    database: String,
    credentials: Option<(String, String)>,
}

/// 拆出 URL 内嵌凭证（`http://user:pass@host:port`）→（纯净 base，凭证）。
fn split_credentials(url: &str) -> (String, Option<(String, String)>) {
    let Some(scheme_end) = url.find("://") else {
        return (url.to_owned(), None);
    };
    let (scheme, rest) = url.split_at(scheme_end + 3);
    let Some(at) = rest.find('@') else {
        return (url.to_owned(), None);
    };
    let (userinfo, host) = rest.split_at(at);
    let host = &host[1..];
    let (user, pass) = userinfo
        .split_once(':')
        .map_or((userinfo, ""), |(u, p)| (u, p));
    (
        format!("{scheme}{host}"),
        Some((user.to_owned(), pass.to_owned())),
    )
}

impl ChClient {
    pub fn new(base_url: &str, database: &str) -> Result<Self, StoreError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;
        let (base, credentials) = split_credentials(base_url.trim_end_matches('/'));
        Ok(Self {
            http,
            base,
            database: database.to_owned(),
            credentials,
        })
    }

    fn with_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.credentials {
            Some((user, pass)) => req
                .header("X-ClickHouse-User", user)
                .header("X-ClickHouse-Key", pass),
            None => req,
        }
    }

    pub async fn ping(&self) -> bool {
        self.http
            .get(format!("{}/ping", self.base))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    async fn post(&self, url: String, body: String) -> Result<String, StoreError> {
        let resp = self
            .with_auth(self.http.post(url))
            .body(body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(StoreError::ChStatus { status, body: text });
        }
        Ok(text)
    }

    /// 执行单条 DDL/DML（database 已限定）。
    pub async fn execute(&self, sql: &str) -> Result<(), StoreError> {
        let url = format!("{}/?database={}", self.base, self.database);
        self.post(url, sql.to_owned()).await?;
        Ok(())
    }

    /// 应用嵌入式 schema（幂等；okapi 库需已存在，容器由 CLICKHOUSE_DB 创建）。
    pub async fn ensure_schema(&self) -> Result<(), StoreError> {
        // 建库兜底（本地/CI 容器已建时是 no-op）
        let url = format!("{}/", self.base);
        self.post(
            url,
            format!("CREATE DATABASE IF NOT EXISTS {}", self.database),
        )
        .await?;

        // Windows 检出（core.autocrlf）会带 CRLF，切不开则整份当作一条语句下发，
        // 而 ClickHouse HTTP 接口拒绝 multi-statement。
        let schema = include_str!("ch_schema.sql").replace('\r', "");
        for statement in schema.split(";\n") {
            let sql = statement.trim();
            if sql.is_empty() || sql.lines().all(|l| l.trim_start().starts_with("--")) {
                continue;
            }
            self.execute(sql).await?;
        }
        Ok(())
    }

    /// 批量写入（JSONEachRow）。`dedup_token` 相同的批次重投会被 CH 去重，
    /// 且经 deduplicate_blocks_in_dependent_materialized_views 传导到全部 MV。
    pub async fn insert_json_each_row(
        &self,
        table: &str,
        rows: &[serde_json::Value],
        dedup_token: &str,
    ) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let query = format!("INSERT INTO {table} FORMAT JSONEachRow");
        let url = format!(
            "{}/?database={}&query={}&insert_deduplication_token={}&deduplicate_blocks_in_dependent_materialized_views=1",
            self.base,
            self.database,
            urlencode(&query),
            urlencode(dedup_token),
        );
        let mut body = String::new();
        for row in rows {
            body.push_str(&row.to_string());
            body.push('\n');
        }
        self.post(url, body).await?;
        Ok(())
    }

    /// 查询（自动追加 FORMAT JSONEachRow 与护栏），返回行对象。
    pub async fn query_json_each_row(
        &self,
        sql: &str,
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let url = format!("{}/?database={}&{}", self.base, self.database, QUERY_GUARD);
        let text = self.post(url, format!("{sql} FORMAT JSONEachRow")).await?;
        let mut rows = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            rows.push(
                serde_json::from_str(line)
                    .map_err(|_| StoreError::InvalidData("clickhouse row not json"))?,
            );
        }
        Ok(rows)
    }
}

fn urlencode(input: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}
