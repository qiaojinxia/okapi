//! ClickHouse 薄客户端：HTTP 接口 + JSONEachRow。
//!
//! 取舍：官方 clickhouse crate（RowBinary）留作 M3 性能优化项；HTTP + JSONEachRow
//! 实现简单、可观察，且原生支持 `insert_deduplication_token`（docs/database.md §3.3
//! 批次幂等）。查询统一带护栏：max_execution_time=15s、max_memory_usage=2GiB、
//! max_query_size=1MiB（默认 256KiB：完整分析源的 SQL 在旧库上已逼近 200KB，再加一个测量子源就会超限）。

mod calendar;
mod population;
pub mod server_tools;

use crate::error::StoreError;
use calendar::calendar_sql;
use std::time::Duration;

/// 进 `request_log_raw` 的账本类型：消费 2、错误 5、退款 6（docs/database.md logs 表，其余留 PG）。
/// 统计读视图按同一份清单（population 规格的 `CLASSIFIED`）判定桶是否完整，写进别的类型会让该桶
/// 永久判为历史不完整，所以投递入口按它拦截。
pub const STATISTICS_LOG_TYPES: [i64; 3] = [2, 5, 6];

const AGGREGATION_GUARD: &str = "max_size_to_preallocate_for_aggregation=8192&max_block_size=8192&max_insert_block_size=8192&min_insert_block_size_rows_for_materialized_views=8192&min_insert_block_size_bytes_for_materialized_views=4194304&input_format_parallel_parsing=0";

/// `join_algorithm`：统计查询把几十个小汇总源 LEFT JOIN 起来，哈希连接每个至少占十来 MiB，
/// 一条看板查询就到 ~375MiB（线上只有几千行时也是），几个面板并发就撞 4GB 机器的内存上限；
/// partial_merge 同一条只用 ~48MiB、耗时不变，结果逐行一致。它不支持的连接回落到 hash。
const QUERY_GUARD: &str = "max_execution_time=15&max_memory_usage=2000000000&max_query_size=1048576&max_size_to_preallocate_for_aggregation=8192&max_size_to_preallocate_for_joins=8192&max_block_size=8192&join_algorithm=partial_merge%2Chash";

#[derive(Clone)]
pub struct ChClient {
    http: reqwest::Client,
    base: String,
    database: String,
    credentials: Option<(String, String)>,
}

/// 拆出 URL 内嵌凭证（`http://user:pass@host:port`）→（纯净 base，凭证）。
///
/// userinfo 到**最后一个** `@` 为止：密码里没转义的 `@`、`/`、`?`、`#` 都留在密码里（部署模板直接把
/// 随机密码拼进 URL，不会替人转义）；用户名与密码再按百分号编码解码（`p%40ss` 就是 `p@ss`）。
/// 去掉凭证后的 base 按 URL 标准校验：只认 http(s)、必须有主机、不许带查询串或片段——这些错写过去
/// 不会报错，只会表现成 ClickHouse「连不上」，和宕机分不出来，所以启动时就拒掉。
fn split_credentials(url: &str) -> Result<(String, Option<(String, String)>), StoreError> {
    let invalid = || StoreError::InvalidData("clickhouse_url");
    let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
    let (credentials, host) = match rest.rsplit_once('@') {
        Some((userinfo, host)) => {
            let (user, pass) = userinfo.split_once(':').unwrap_or((userinfo, ""));
            (Some((percent_decode(user), percent_decode(pass))), host)
        }
        None => (None, rest),
    };
    let base = format!("{scheme}://{host}");
    let parsed = reqwest::Url::parse(&base).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none_or(str::is_empty)
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok((base.trim_end_matches('/').to_owned(), credentials))
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl ChClient {
    pub fn new(base_url: &str, database: &str) -> Result<Self, StoreError> {
        // ClickHouse closes idle keep-alive connections after `keep_alive_timeout`
        // (10s by default, 3s on older servers). Reusing one the server is closing
        // fails as "error sending request"; evict idle connections before that.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(2))
            .build()?;
        let (base, credentials) = split_credentials(base_url.trim())?;
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

    fn query_url(&self, zone: &str, params: &[(&str, &str)]) -> String {
        let mut url = format!(
            "{}/?database={}&{}&session_timezone={}",
            self.base,
            self.database,
            QUERY_GUARD,
            urlencode(zone)
        );
        for (name, value) in params {
            use std::fmt::Write as _;
            let _ = write!(
                url,
                "&param_{}={}",
                urlencode(name),
                urlencode(&escape_parameter(value))
            );
        }
        url
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
        let text = resp.text().await?;
        if !(200..300).contains(&status) {
            if let Some(code) = history_error(&text) {
                return Err(StoreError::InvalidData(code));
            }
            return Err(StoreError::ChStatus { status, body: text });
        }
        Ok(text)
    }

    /// 执行单条 DDL/DML（database 已限定）。
    pub async fn execute(&self, sql: &str) -> Result<(), StoreError> {
        let url = format!(
            "{}/?database={}&session_timezone=UTC&{}",
            self.base, self.database, AGGREGATION_GUARD
        );
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
        for statement in population::schema()? {
            self.execute(&statement).await?;
        }
        server_tools::schema::ensure(self).await?;
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
            "{}/?database={}&query={}&insert_deduplication_token={}&deduplicate_blocks_in_dependent_materialized_views=1&session_timezone=UTC&{}",
            self.base,
            self.database,
            urlencode(&query),
            urlencode(dedup_token),
            AGGREGATION_GUARD,
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
        self.query_with_params(sql, &[]).await
    }

    /// 带绑定参数的查询：SQL 里写 `{name:String}` 占位，值经 `param_name=` 传递，
    /// 标量值先按 HTTP 参数的 escaped 格式编码，再做 URL 编码；服务端解析类型。
    /// 调用方传原始字符串，不预先转义（含字面量 `\N`）。
    ///
    /// 存在的理由是**日志检索要吃用户输入的字符串**（模型名/错误码/请求 ID）。
    /// 看板端点只拼 clamp 过的整数所以能用 `format!`，检索面不行——
    /// 与其在 Rust 侧手写转义（每加一处过滤都要重新审一遍），不如把转义交给服务端。
    pub async fn query_with_params(
        &self,
        sql: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let zone = crate::timezone::machine_timezone()?;
        let sql = calendar_sql(sql, zone);
        let referenced = population::referenced(&sql)?;
        // Keep complete modern sources small. Expanding dozens of history views
        // solely to discover empty fallbacks can exhaust memory before scanning.
        // No cached proof: every read checks record coverage at each full grain.
        let complete = if referenced.is_empty() {
            Vec::new()
        } else {
            let url = self.query_url(zone, params);
            let rows = self
                .post(
                    url,
                    format!(
                        "{} FORMAT JSONEachRow",
                        population::coverage(&sql, &referenced)
                    ),
                )
                .await?;
            parse_rows(&rows)?
                .into_iter()
                .filter(|row| row["missing"] == "0" || row["missing"] == 0)
                .filter_map(|row| row["name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        let names = complete.iter().map(String::as_str).collect::<Vec<_>>();
        let sql = population::sql(&sql, &names)?;
        let url = self.query_url(zone, params);
        let text = self.post(url, format!("{sql} FORMAT JSONEachRow")).await?;
        parse_rows(&text)
    }
}

/// A query can fail after HTTP headers and some rows have already been sent.
/// Reject the entire response, including coverage probes, on a streamed error.
fn parse_rows(text: &str) -> Result<Vec<serde_json::Value>, StoreError> {
    let mut rows = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let row: serde_json::Value = serde_json::from_str(line)
            .map_err(|_| StoreError::InvalidData("clickhouse row not json"))?;
        if let Some(exception) = row.get("exception").and_then(serde_json::Value::as_str) {
            if let Some(code) = history_error(exception) {
                return Err(StoreError::InvalidData(code));
            }
            return Err(StoreError::ChStatus {
                status: 200,
                body: exception.to_owned(),
            });
        }
        rows.push(row);
    }
    Ok(rows)
}

/// A query echo can contain guard literals without having triggered the guard.
/// Only the throwIf exception code and its primary message prove missing history.
fn history_error(text: &str) -> Option<&'static str> {
    let envelope = serde_json::from_str::<serde_json::Value>(text).ok();
    let text = envelope
        .as_ref()
        .and_then(|row| row.get("exception"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(text);
    let message = text.trim_start().strip_prefix("Code: 395.")?.trim_start();
    let message = message
        .strip_prefix("DB::Exception:")
        .unwrap_or(message)
        .trim_start();
    [
        "statistics_request_history_incomplete",
        "statistics_calendar_history_incomplete",
    ]
    .into_iter()
    .find(|code| {
        message.strip_prefix(code).is_some_and(|suffix| {
            suffix.is_empty()
                || suffix.starts_with([':', '.'])
                || suffix.starts_with(char::is_whitespace)
        })
    })
}

// ClickHouse HTTP parameters are parsed as escaped text *after* URL decoding.
// Merely encoding a tab as %09 or a backslash as %5C changes/truncates the value.
fn escape_parameter(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\0' => escaped.push_str("\\0"),
            _ => escaped.push(ch),
        }
    }
    escaped
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

#[cfg(test)]
mod url_tests {
    use super::split_credentials;

    #[test]
    fn credentials_are_split_by_the_url_standard_and_decoded() {
        let (base, creds) = split_credentials("http://okapi:p%40ss%3Aw@ch.local:8123/").unwrap();
        assert_eq!(base, "http://ch.local:8123");
        assert_eq!(creds, Some(("okapi".into(), "p@ss:w".into())));
        // 没转义的 @ 也落在密码里，而不是把一半密码当成主机
        let (base, creds) = split_credentials("http://okapi:p@ss@ch.local:8123").unwrap();
        assert_eq!(base, "http://ch.local:8123");
        assert_eq!(creds, Some(("okapi".into(), "p@ss".into())));
        let (_, creds) = split_credentials("http://okapi:a/b?c#d@ch.local:8123").unwrap();
        assert_eq!(creds, Some(("okapi".into(), "a/b?c#d".into())));
        let (base, creds) = split_credentials("https://ch.local").unwrap();
        assert_eq!((base.as_str(), creds), ("https://ch.local", None));
    }

    #[test]
    fn urls_that_would_only_ever_look_unreachable_are_refused() {
        for url in [
            "ch.local:8123",
            "tcp://ch.local:9000",
            "http://ch.local:8123/?database=x",
            "http://:8123",
            "",
        ] {
            assert!(split_credentials(url).is_err(), "{url}");
        }
    }
}
