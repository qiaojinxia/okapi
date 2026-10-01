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
        let url = format!(
            "{}/?database={}&session_timezone=UTC",
            self.base, self.database
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
            "{}/?database={}&query={}&insert_deduplication_token={}&deduplicate_blocks_in_dependent_materialized_views=1&session_timezone=UTC",
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

/// Daily UTC aggregates cannot be relabeled as local days. Rebucket the retained
/// hourly facts for financial/token totals and independent retained calendar
/// dimensions. Existing UTC MVs and their history remain untouched.
fn calendar_sql(sql: &str, zone: &str) -> String {
    let mut sql = sql.to_owned();
    if zone != "UTC" && zone != "Etc/UTC" {
        for statement in include_str!("ch_schema.sql").split(";\n") {
            let Some(start) = statement.find("CREATE MATERIALIZED VIEW IF NOT EXISTS ") else {
                continue;
            };
            let name = statement[start + "CREATE MATERIALIZED VIEW IF NOT EXISTS ".len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default();
            if !name.ends_with("_day") || !sql.contains(name) {
                continue;
            }
            let Some((_, select)) = statement.split_once("AS SELECT") else {
                continue;
            };
            let mut select = format!("SELECT{select}");
            if matches!(
                name,
                "mv_user_day"
                    | "mv_apikey_day"
                    | "mv_user_model_day"
                    | "mv_key_model_day"
                    | "mv_group_day"
            ) {
                select = select.replace("countState() AS requests", "countMergeState(requests) AS requests")
                    .replace("FROM request_log_raw", "FROM (SELECT hour AS ts,user_id,api_key_id,group_code,model,countMergeState(requests) AS requests,sumMerge(prompt_tokens) AS prompt_tokens,sumMerge(cached_tokens) AS cached_tokens,sumMerge(completion_tokens) AS completion_tokens,sumMerge(reasoning_tokens) AS reasoning_tokens,sumMerge(amount) AS amount_micro,sumMerge(discount) AS discount_micro,sumMerge(amount)+sumMerge(discount) AS original_amount_micro,sumMerge(upstream_cost) AS upstream_cost_micro,sumMerge(errors) AS is_error FROM mv_cube_hour GROUP BY hour,user_id,api_key_id,group_code,model)");
            } else {
                select = match name {
                    "mv_client_day" => "SELECT client_type,toDate(hour) AS day,countMergeState(requests) AS requests,sumMergeState(tokens) AS tokens,sumMergeState(amount) AS amount,sumMergeState(errors) AS errors,uniqMergeState(users) AS users FROM mv_calendar_client_hour GROUP BY client_type,day".into(),
                    "mv_cache_write_day" => "SELECT user_id,api_key_id,model,toDate(hour) AS day,sumMergeState(write_tokens) AS write_tokens,countIfMergeState(known_requests) AS known_requests FROM mv_calendar_cache_write_hour GROUP BY user_id,api_key_id,model,day".into(),
                    "mv_cache_reporting_day" => "SELECT user_id,api_key_id,model,toDate(hour) AS day,countIfMergeState(read_known) AS read_known,countIfMergeState(write_known) AS write_known,sumMergeState(write_tokens) AS write_tokens FROM mv_calendar_cache_reporting_hour GROUP BY user_id,api_key_id,model,day".into(),
                    _ => select,
                };
            }
            // Keep qualified references valid by retaining the original table alias.
            for keyword in ["FROM", "JOIN"] {
                sql = replace_identifier(
                    &sql,
                    &format!("{keyword} {name}"),
                    &format!("{keyword} ({select}) AS {name}"),
                );
            }
        }
    }
    for column in ["ts", "hour", "ts5"] {
        sql = sql.replace(
            &format!("toDate({column})"),
            &format!("toDate({column}, '{zone}')"),
        );
    }
    for column in ["hour", "ts5"] {
        sql = sql.replace(
            &format!("toString({column})"),
            &format!("toString(toTimeZone({column}, '{zone}'))"),
        );
    }
    sql = sql.replace(
        "toString(toStartOfHour(hour))",
        &format!("toString(toTimeZone(toStartOfHour(hour), '{zone}'))"),
    );
    sql = sql.replace("timezone()", &format!("'{zone}'"));
    sql = sql.replace("today()", &format!("toDate(now(), '{zone}')"));
    sql = sql.replace(
        "toStartOfDay(day)",
        &format!("toStartOfDay(toDateTime(day, '{zone}'))"),
    );
    sql
}

fn replace_identifier(sql: &str, name: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0;
    for (start, _) in sql.match_indices(name) {
        let end = start + name.len();
        let identifier = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if start > 0 && identifier(sql.as_bytes()[start - 1])
            || end < sql.len() && identifier(sql.as_bytes()[end])
        {
            continue;
        }
        out.push_str(&sql[cursor..start]);
        out.push_str(replacement);
        cursor = end;
    }
    out.push_str(&sql[cursor..]);
    out
}

#[cfg(test)]
mod calendar_tests {
    use super::*;
    #[test]
    fn rebuckets_from_hourly_states_instead_of_relabeling_utc_days() {
        let sql = calendar_sql(
            "SELECT sumMerge(amount) FROM mv_user_day WHERE day=today()",
            "Asia/Shanghai",
        );
        assert!(sql.contains("FROM mv_cube_hour"));
        assert!(sql.contains("toDate(ts, 'Asia/Shanghai')"));
        assert!(!sql.contains("FROM mv_user_day"));
        assert_eq!(
            replace_identifier(
                "mv_user_day_extra mv_user_day",
                "mv_user_day",
                "replacement"
            ),
            "mv_user_day_extra replacement"
        );
    }
}
