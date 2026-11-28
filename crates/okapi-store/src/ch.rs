//! ClickHouse 薄客户端：HTTP 接口 + JSONEachRow。
//!
//! 取舍：官方 clickhouse crate（RowBinary）留作 M3 性能优化项；HTTP + JSONEachRow
//! 实现简单、可观察，且原生支持 `insert_deduplication_token`（docs/database.md §3.3
//! 批次幂等）。查询统一带护栏：max_execution_time=15s、max_memory_usage=2GiB、
//! max_query_size=1MiB（默认 256KiB：完整分析源的 SQL 在旧库上已逼近 200KB，再加一个测量子源就会超限）。

mod calendar;
mod population;
pub mod row;
pub mod server_tools;

use crate::error::StoreError;
use calendar::calendar_sql;
use std::time::Duration;

/// 进 `request_log_raw` 的账本类型：消费 2、错误 5、退款 6（docs/database.md logs 表，其余留 PG）。
/// 统计读视图按同一份清单（population 规格的 `CLASSIFIED`）判定桶是否完整，写进别的类型会让该桶
/// 永久判为历史不完整，所以投递入口按它拦截。
pub const STATISTICS_LOG_TYPES: [i64; 3] = [2, 5, 6];

const AGGREGATION_GUARD: &str = "max_size_to_preallocate_for_aggregation=8192&max_block_size=8192&max_insert_block_size=8192&min_insert_block_size_rows_for_materialized_views=8192&min_insert_block_size_bytes_for_materialized_views=4194304&input_format_parallel_parsing=0&max_threads=2";

/// `join_algorithm`：partial_merge 限制并发分析查询的连接内存，不支持的连接回落到 hash；
/// `enable_http_compression=1` 配合 `Accept-Encoding: gzip` 启用查询响应压缩。
const QUERY_GUARD: &str = "max_execution_time=15&max_memory_usage=2000000000&max_query_size=1048576&max_size_to_preallocate_for_aggregation=8192&max_size_to_preallocate_for_joins=8192&max_block_size=8192&join_algorithm=partial_merge%2Chash&max_threads=2&enable_http_compression=1";

/// 批写 JSONEachRow 超过此阈值即启用 `Content-Encoding: gzip` 压缩上行。
const GZIP_REQUEST_THRESHOLD: usize = 512;

/// ClickHouse 并发查询闸默认值及上下界（大查询 weight = 2，下限必须 >= 2 以防永久阻塞）。
const DEFAULT_CH_MAX_CONCURRENCY: usize = 4;
const MIN_CH_MAX_CONCURRENCY: usize = 2;
const MAX_CH_MAX_CONCURRENCY: usize = 64;

#[derive(Clone)]
pub struct ChClient {
    http: reqwest::Client,
    base: String,
    database: String,
    credentials: Option<(String, String)>,
    query_gate: std::sync::Arc<tokio::sync::Semaphore>,
    archive_table_ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    archive_table_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
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
        let max_concurrency = crate::env_config::number(
            "OKAPI_CH_MAX_CONCURRENCY",
            DEFAULT_CH_MAX_CONCURRENCY,
            |v| (MIN_CH_MAX_CONCURRENCY..=MAX_CH_MAX_CONCURRENCY).contains(v),
        );
        Ok(Self {
            http,
            base,
            database: database.to_owned(),
            credentials,
            query_gate: std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrency)),
            archive_table_ready: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            archive_table_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// 单飞幂等确保对话存档表存在（按 `ChClient` 实例隔离，避免并发 DDL 风暴与跨库污染）。
    pub(crate) async fn ensure_archive_table(&self, ddl: &str) -> Result<(), StoreError> {
        use std::sync::atomic::Ordering;
        if self.archive_table_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let _guard = self.archive_table_lock.lock().await;
        if self.archive_table_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        self.execute(ddl).await?;
        self.archive_table_ready.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn invalidate_archive_table(&self) {
        self.archive_table_ready
            .store(false, std::sync::atomic::Ordering::Release);
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
        self.post_raw(url, body.into_bytes(), false, false).await
    }

    async fn post_raw(
        &self,
        url: String,
        body: Vec<u8>,
        gzip_request: bool,
        accept_gzip: bool,
    ) -> Result<String, StoreError> {
        let mut req = self.with_auth(self.http.post(url));
        if accept_gzip {
            req = req.header(reqwest::header::ACCEPT_ENCODING, "gzip");
        }
        let payload = if gzip_request && body.len() >= GZIP_REQUEST_THRESHOLD {
            let compressed = gzip_compress(&body)?;
            req = req.header(reqwest::header::CONTENT_ENCODING, "gzip");
            compressed
        } else {
            body
        };
        let resp = req.body(payload).send().await?;
        let status = resp.status().as_u16();
        let is_gzip = resp
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("gzip"));
        let bytes = resp.bytes().await?;
        let text = if is_gzip {
            gzip_decompress(&bytes)?
        } else {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| StoreError::InvalidData("clickhouse_response_not_utf8"))?
        };
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

    async fn execute_concurrent(&self, statements: Vec<String>) -> Result<(), StoreError> {
        if statements.is_empty() {
            return Ok(());
        }
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut tasks = tokio::task::JoinSet::new();
        for sql in statements {
            let client = self.clone();
            let gate = std::sync::Arc::clone(&gate);
            tasks.spawn(async move {
                let _permit = gate
                    .acquire_owned()
                    .await
                    .map_err(|_| StoreError::InvalidData("clickhouse_schema_gate_closed"))?;
                client.execute(&sql).await
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.map_err(|err| {
                StoreError::InvalidData(if err.is_panic() {
                    "clickhouse_schema_task_panic"
                } else {
                    "clickhouse_schema_task_cancelled"
                })
            })??;
        }
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
        let mut base_ddl = Vec::new();
        let mut materialized_views = Vec::new();
        for statement in schema.split(";\n") {
            let sql = statement.trim();
            if sql.is_empty() || sql.lines().all(|l| l.trim_start().starts_with("--")) {
                continue;
            }
            if sql.lines().any(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("--") && trimmed.starts_with("CREATE MATERIALIZED VIEW")
            }) {
                materialized_views.push(sql.to_owned());
            } else {
                base_ddl.push(sql.to_owned());
            }
        }
        for sql in base_ddl {
            self.execute(&sql).await?;
        }
        let read_views = population::read_views()?;
        self.execute_concurrent(materialized_views).await?;
        self.execute_concurrent(read_views).await?;
        server_tools::schema::ensure(self).await?;
        // ch_schema.sql 已包含 conversation_archives 建表 DDL，跳过首次存档写入的冗余建表检查
        self.archive_table_ready
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// 批量写入（JSONEachRow，>=512B 自动 gzip 压缩上行）。`dedup_token` 相同的批次重投会被
    /// CH 去重，且经 deduplicate_blocks_in_dependent_materialized_views 传导到全部 MV。
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
        let body = encode_json_each_row(rows)?;
        self.post_raw(url, body, true, false).await?;
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
        let zone = crate::timezone::reporting_timezone()?;
        let sql = calendar_sql(sql, zone);
        let sql = population::sql(&sql)?;
        let url = self.query_url(zone, params);
        let weight: u32 = if sql.len() >= 16 * 1024 { 2 } else { 1 };
        let text = {
            let _permit = self
                .query_gate
                .acquire_many(weight)
                .await
                .map_err(|_| StoreError::InvalidData("clickhouse_query_gate_closed"))?;
            self.post_raw(
                url,
                format!("{sql} FORMAT JSONEachRow").into_bytes(),
                false,
                true,
            )
            .await?
        };
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
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if let Ok(row) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(exception) = row.get("exception").and_then(serde_json::Value::as_str)
            && let Some(code) = history_exception_code(exception)
        {
            return Some(code);
        }
    }
    history_exception_code(text)
}

fn history_exception_code(text: &str) -> Option<&'static str> {
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

fn encode_json_each_row(rows: &[serde_json::Value]) -> Result<Vec<u8>, StoreError> {
    let mut body = Vec::with_capacity(rows.len().saturating_mul(256));
    for row in rows {
        serde_json::to_writer(&mut body, row)
            .map_err(|_| StoreError::InvalidData("clickhouse_json_encode_failed"))?;
        body.push(b'\n');
    }
    Ok(body)
}

fn gzip_compress(input: &[u8]) -> Result<Vec<u8>, StoreError> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder
        .write_all(input)
        .map_err(|_| StoreError::InvalidData("clickhouse_gzip_encode_failed"))?;
    encoder
        .finish()
        .map_err(|_| StoreError::InvalidData("clickhouse_gzip_encode_failed"))
}

fn gzip_decompress(input: &[u8]) -> Result<String, StoreError> {
    use std::io::Read as _;
    let mut decoder = flate2::read::GzDecoder::new(input);
    let mut text = String::new();
    decoder
        .read_to_string(&mut text)
        .map_err(|_| StoreError::InvalidData("clickhouse_gzip_invalid"))?;
    Ok(text)
}

#[cfg(test)]
mod url_tests {
    use super::{encode_json_each_row, gzip_compress, gzip_decompress, split_credentials};

    #[test]
    fn encodes_json_each_row_directly_into_ndjson_bytes() {
        let rows = vec![
            serde_json::json!({"id": 1, "msg": "hello"}),
            serde_json::json!({"id": 2, "msg": "你好"}),
        ];
        let bytes = encode_json_each_row(&rows).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[0]).unwrap(),
            rows[0]
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[1]).unwrap(),
            rows[1]
        );
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn gzip_round_trips_and_rejects_truncated_payloads() {
        let sample = "{\"a\":1}\n".repeat(128);
        let compressed = gzip_compress(sample.as_bytes()).unwrap();
        assert!(compressed.len() < sample.len() / 2);
        assert_eq!(gzip_decompress(&compressed).unwrap(), sample);
        assert!(gzip_decompress(&compressed[..compressed.len() / 2]).is_err());
    }

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
