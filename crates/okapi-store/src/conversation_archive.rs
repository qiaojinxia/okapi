//! 对话内容存档的双后端存储层：ClickHouse 启用时优先落 ClickHouse（ZSTD 压缩 + 独立表），
//! 未启用 ClickHouse（或 ClickHouse 临时不可用）时自动落 PostgreSQL（TOAST 压缩）。
//!
//! 保留策略由 `settings.conversation_archive.retention_days` 控制：0 = 永久保留，>0 由 worker
//! 定时清理（PG 分批 DELETE + CH 异步 DELETE mutation）。

use crate::{ChClient, StoreError};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const CH_CREATE_TABLE_SQL: &str = r"CREATE TABLE IF NOT EXISTS conversation_archives (
    created_at      DateTime64(3, 'UTC'),
    request_id      UUID,
    user_id         UInt64,
    api_key_id      UInt64,
    channel_id      UInt32 DEFAULT 0,
    model           LowCardinality(String),
    requested_model LowCardinality(String) DEFAULT '',
    endpoint        LowCardinality(String) DEFAULT '',
    is_stream       UInt8 DEFAULT 0,
    status_code     UInt16 DEFAULT 200,
    error_code      LowCardinality(String) DEFAULT '',
    request_body    String CODEC(ZSTD(3)),
    response_body   String CODEC(ZSTD(3)),
    response_text   String CODEC(ZSTD(3)),
    reasoning_text  String CODEC(ZSTD(3)),
    tool_calls      String CODEC(ZSTD(3)),
    truncated       UInt8 DEFAULT 0
) ENGINE = ReplacingMergeTree(created_at)
PARTITION BY toYYYYMMDD(created_at)
ORDER BY (user_id, created_at, request_id)
SETTINGS index_granularity = 8192, non_replicated_deduplication_window = 1000";

const PG_PRUNE_BATCH: i64 = 5000;
const PG_PRUNE_MAX_ROUNDS: usize = 20;
const PREVIEW_CHARS: usize = 160;

/// 完整的单笔对话存档记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveRecord {
    pub request_id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub channel_id: i64,
    pub model: String,
    pub requested_model: String,
    pub endpoint: String,
    pub is_stream: bool,
    pub status_code: i16,
    pub error_code: String,
    pub request_body: String,
    pub response_body: String,
    pub response_text: String,
    pub reasoning_text: String,
    pub tool_calls: String,
    pub truncated: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub storage: String,
}

/// 列表页用的轻量摘要（避免把几百 KB 原文一次性拉给列表页）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSummary {
    pub request_id: Uuid,
    pub user_id: i64,
    pub api_key_id: i64,
    pub channel_id: i64,
    pub model: String,
    pub requested_model: String,
    pub endpoint: String,
    pub is_stream: bool,
    pub status_code: i16,
    pub error_code: String,
    pub request_preview: String,
    pub response_preview: String,
    pub truncated: bool,
    pub created_at: DateTime<Utc>,
    pub storage: String,
}

/// 列表检索过滤条件。
#[derive(Debug, Clone, Default)]
pub struct ArchiveFilter {
    pub user_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub model: Option<String>,
    pub request_id: Option<Uuid>,
    pub search: Option<String>,
    pub hours: Option<u32>,
    pub limit: u32,
    pub offset: u32,
}

/// 存档存储状态统计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveStats {
    pub backend: String,
    pub clickhouse_enabled: bool,
    pub total_count: u64,
    pub pg_count: u64,
    pub ch_count: Option<u64>,
    pub oldest_at: Option<DateTime<Utc>>,
    pub newest_at: Option<DateTime<Utc>>,
}

/// 一次清理任务的执行结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PruneReport {
    pub pg_deleted: u64,
    pub ch_deleted_requested: bool,
    pub cutoff: Option<DateTime<Utc>>,
}

/// 幂等确保 ClickHouse `conversation_archives` 表存在（按 `ChClient` 单飞缓存，避免并发 DDL 风暴）。
pub async fn ensure_ch_table(ch: &ChClient) -> Result<(), StoreError> {
    ch.ensure_archive_table(CH_CREATE_TABLE_SQL).await
}

fn fmt_ch_ts(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

fn parse_ch_ts(raw: &str) -> DateTime<Utc> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return dt.with_timezone(&Utc);
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw.trim(), fmt) {
            return naive.and_utc();
        }
    }
    DateTime::<Utc>::UNIX_EPOCH
}

fn preview_text(input: &str) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for (idx, ch) in collapsed.chars().enumerate() {
        if idx >= PREVIEW_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// 写入一条对话存档：ClickHouse 启用时优先写 ClickHouse，失败时自动降级到 PostgreSQL；
/// 未启用 ClickHouse 时直接写 PostgreSQL。返回实际落盘后端名（`"clickhouse"` 或 `"postgres"`）。
pub async fn insert(
    pg: &PgPool,
    ch: Option<&ChClient>,
    record: &ArchiveRecord,
) -> Result<&'static str, StoreError> {
    insert_batch(pg, ch, std::slice::from_ref(record)).await
}

/// 批量写入对话存档：ClickHouse 启用时以单次多行 `JSONEachRow` 写入（避免高频单行写入引发
/// MergeTree part 碎片化与合并放大），失败时自动整批降级为 PostgreSQL `UNNEST` 批量写。
pub async fn insert_batch(
    pg: &PgPool,
    ch: Option<&ChClient>,
    records: &[ArchiveRecord],
) -> Result<&'static str, StoreError> {
    if records.is_empty() {
        return Ok(if ch.is_some() {
            "clickhouse"
        } else {
            "postgres"
        });
    }
    if let Some(ch) = ch {
        match insert_ch_batch(ch, records).await {
            Ok(()) => return Ok("clickhouse"),
            Err(err) => {
                ch.invalidate_archive_table();
                tracing::warn!(
                    count = records.len(),
                    first_request_id = %records[0].request_id,
                    error = %err,
                    "ClickHouse 对话存档批量写入失败，降级写入 PostgreSQL"
                );
            }
        }
    }
    insert_pg_batch(pg, records).await?;
    Ok("postgres")
}

fn archive_row_to_ch_json(r: &ArchiveRecord) -> Value {
    json!({
        "created_at": fmt_ch_ts(r.created_at),
        "request_id": r.request_id.to_string(),
        "user_id": u64::try_from(r.user_id.max(0)).unwrap_or(0),
        "api_key_id": u64::try_from(r.api_key_id.max(0)).unwrap_or(0),
        "channel_id": u32::try_from(r.channel_id.max(0)).unwrap_or(0),
        "model": r.model,
        "requested_model": r.requested_model,
        "endpoint": r.endpoint,
        "is_stream": u8::from(r.is_stream),
        "status_code": u16::try_from(r.status_code.max(0)).unwrap_or(200),
        "error_code": r.error_code,
        "request_body": r.request_body,
        "response_body": r.response_body,
        "response_text": r.response_text,
        "reasoning_text": r.reasoning_text,
        "tool_calls": r.tool_calls,
        "truncated": u8::from(r.truncated),
    })
}

fn batch_dedup_token(records: &[ArchiveRecord]) -> String {
    if let [single] = records {
        return format!("conv:{}", single.request_id);
    }
    // FNV-1a 64-bit over all request_ids for deterministic retry dedup per batch.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for r in records {
        for &b in r.request_id.as_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!(
        "conv-batch:{}:{}:{hash:016x}",
        records[0].request_id,
        records.len()
    )
}

async fn insert_ch_batch(ch: &ChClient, records: &[ArchiveRecord]) -> Result<(), StoreError> {
    ensure_ch_table(ch).await?;
    let rows: Vec<Value> = records.iter().map(archive_row_to_ch_json).collect();
    let token = batch_dedup_token(records);
    ch.insert_json_each_row("conversation_archives", &rows, &token)
        .await
}

async fn insert_pg_batch(pg: &PgPool, records: &[ArchiveRecord]) -> Result<(), StoreError> {
    if let [single] = records {
        return insert_pg_one(pg, single).await;
    }
    let len = records.len();
    let mut request_ids = Vec::with_capacity(len);
    let mut user_ids = Vec::with_capacity(len);
    let mut api_key_ids = Vec::with_capacity(len);
    let mut channel_ids = Vec::with_capacity(len);
    let mut models = Vec::with_capacity(len);
    let mut requested_models = Vec::with_capacity(len);
    let mut endpoints = Vec::with_capacity(len);
    let mut is_streams = Vec::with_capacity(len);
    let mut status_codes = Vec::with_capacity(len);
    let mut error_codes = Vec::with_capacity(len);
    let mut request_bodies = Vec::with_capacity(len);
    let mut response_bodies = Vec::with_capacity(len);
    let mut response_texts = Vec::with_capacity(len);
    let mut reasoning_texts = Vec::with_capacity(len);
    let mut tool_calls = Vec::with_capacity(len);
    let mut truncateds = Vec::with_capacity(len);
    let mut created_ats = Vec::with_capacity(len);

    for r in records {
        request_ids.push(r.request_id);
        user_ids.push(r.user_id);
        api_key_ids.push(r.api_key_id);
        channel_ids.push(r.channel_id);
        models.push(r.model.as_str());
        requested_models.push(r.requested_model.as_str());
        endpoints.push(r.endpoint.as_str());
        is_streams.push(r.is_stream);
        status_codes.push(r.status_code);
        error_codes.push(r.error_code.as_str());
        request_bodies.push(r.request_body.as_str());
        response_bodies.push(r.response_body.as_str());
        response_texts.push(r.response_text.as_str());
        reasoning_texts.push(r.reasoning_text.as_str());
        tool_calls.push(r.tool_calls.as_str());
        truncateds.push(r.truncated);
        created_ats.push(r.created_at);
    }

    sqlx::query(
        r"INSERT INTO conversation_archives (
            request_id, user_id, api_key_id, channel_id, model, requested_model,
            endpoint, is_stream, status_code, error_code,
            request_body, response_body, response_text, reasoning_text, tool_calls,
            truncated, created_at
          )
          SELECT * FROM UNNEST(
            $1::uuid[], $2::bigint[], $3::bigint[], $4::bigint[], $5::text[], $6::text[],
            $7::text[], $8::bool[], $9::smallint[], $10::text[],
            $11::text[], $12::text[], $13::text[], $14::text[], $15::text[],
            $16::bool[], $17::timestamptz[]
          )
          ON CONFLICT (request_id) DO NOTHING",
    )
    .bind(request_ids)
    .bind(user_ids)
    .bind(api_key_ids)
    .bind(channel_ids)
    .bind(models)
    .bind(requested_models)
    .bind(endpoints)
    .bind(is_streams)
    .bind(status_codes)
    .bind(error_codes)
    .bind(request_bodies)
    .bind(response_bodies)
    .bind(response_texts)
    .bind(reasoning_texts)
    .bind(tool_calls)
    .bind(truncateds)
    .bind(created_ats)
    .execute(pg)
    .await?;
    Ok(())
}

async fn insert_pg_one(pg: &PgPool, r: &ArchiveRecord) -> Result<(), StoreError> {
    sqlx::query(
        r"INSERT INTO conversation_archives (
            request_id, user_id, api_key_id, channel_id, model, requested_model,
            endpoint, is_stream, status_code, error_code,
            request_body, response_body, response_text, reasoning_text, tool_calls,
            truncated, created_at
          ) VALUES (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10,
            $11, $12, $13, $14, $15,
            $16, $17
          )
          ON CONFLICT (request_id) DO NOTHING",
    )
    .bind(r.request_id)
    .bind(r.user_id)
    .bind(r.api_key_id)
    .bind(r.channel_id)
    .bind(&r.model)
    .bind(&r.requested_model)
    .bind(&r.endpoint)
    .bind(r.is_stream)
    .bind(r.status_code)
    .bind(&r.error_code)
    .bind(&r.request_body)
    .bind(&r.response_body)
    .bind(&r.response_text)
    .bind(&r.reasoning_text)
    .bind(&r.tool_calls)
    .bind(r.truncated)
    .bind(r.created_at)
    .execute(pg)
    .await?;
    Ok(())
}

/// 按 `request_id` 读取完整对话存档；启用 CH 时先查 CH 再兜底 PG（兼容切换与降级落盘）。
pub async fn get_by_request_id(
    pg: &PgPool,
    ch: Option<&ChClient>,
    request_id: Uuid,
    owner_user_id: Option<i64>,
) -> Result<Option<ArchiveRecord>, StoreError> {
    if let Some(ch) = ch {
        match get_ch(ch, request_id, owner_user_id).await {
            Ok(Some(rec)) => return Ok(Some(rec)),
            // 不在 ClickHouse：可能是 ClickHouse 写失败时落进 PG 的那几条
            Ok(None) => {}
            Err(err) if err.is_ch_unavailable() => {
                tracing::warn!(error = %err, "ClickHouse 不可用，对话存档详情改查 PostgreSQL");
            }
            Err(err) => return Err(err),
        }
    }
    get_pg(pg, request_id, owner_user_id).await
}

// ClickHouse 的 SELECT 别名在 WHERE / ORDER BY 里同样可见：取列的文本形式（`toString(created_at)` /
// `toString(request_id)`）时别名不能和列同名，否则 `created_at >= now64(3) - …`、`request_id = toUUID(…)`
// 比较的是字符串，整条查询报 ILLEGAL_TYPE_OF_ARGUMENT，于是回退到（ClickHouse 在用时为空的）PG 表，
// 列表「暂无数据」、详情 404。文本列一律叫 `*_text`。
use crate::ch::row::int as ch_i64;

fn ch_str(v: &Value, key: &str) -> String {
    crate::ch::row::text(v, key).to_owned()
}

async fn get_ch(
    ch: &ChClient,
    request_id: Uuid,
    owner_user_id: Option<i64>,
) -> Result<Option<ArchiveRecord>, StoreError> {
    ensure_ch_table(ch).await?;
    let mut sql = String::from(
        "SELECT toString(created_at) AS created_at_text, toString(request_id) AS request_id_text, \
         user_id, api_key_id, channel_id, model, requested_model, endpoint, is_stream, \
         status_code, error_code, request_body, response_body, response_text, \
         reasoning_text, tool_calls, truncated \
         FROM conversation_archives FINAL \
         WHERE request_id = toUUID({req_id:String})",
    );
    let req_str = request_id.to_string();
    let uid_str;
    let mut params: Vec<(&str, &str)> = vec![("req_id", &req_str)];
    if let Some(uid) = owner_user_id {
        uid_str = uid.max(0).to_string();
        sql.push_str(" AND user_id = toUInt64({uid:String})");
        params.push(("uid", &uid_str));
    }
    sql.push_str(" LIMIT 1");
    let rows = ch.query_with_params(&sql, &params).await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let parsed_id = Uuid::parse_str(&ch_str(row, "request_id_text")).unwrap_or(request_id);
    Ok(Some(ArchiveRecord {
        request_id: parsed_id,
        user_id: ch_i64(row, "user_id"),
        api_key_id: ch_i64(row, "api_key_id"),
        channel_id: ch_i64(row, "channel_id"),
        model: ch_str(row, "model"),
        requested_model: ch_str(row, "requested_model"),
        endpoint: ch_str(row, "endpoint"),
        is_stream: ch_i64(row, "is_stream") != 0,
        status_code: i16::try_from(ch_i64(row, "status_code")).unwrap_or(200),
        error_code: ch_str(row, "error_code"),
        request_body: ch_str(row, "request_body"),
        response_body: ch_str(row, "response_body"),
        response_text: ch_str(row, "response_text"),
        reasoning_text: ch_str(row, "reasoning_text"),
        tool_calls: ch_str(row, "tool_calls"),
        truncated: ch_i64(row, "truncated") != 0,
        created_at: parse_ch_ts(&ch_str(row, "created_at_text")),
        storage: "clickhouse".into(),
    }))
}

async fn get_pg(
    pg: &PgPool,
    request_id: Uuid,
    owner_user_id: Option<i64>,
) -> Result<Option<ArchiveRecord>, StoreError> {
    let row = sqlx::query(
        r"SELECT request_id, user_id, api_key_id, channel_id, model, requested_model,
                 endpoint, is_stream, status_code, error_code,
                 request_body, response_body, response_text, reasoning_text, tool_calls,
                 truncated, created_at
          FROM conversation_archives
          WHERE request_id = $1 AND ($2::bigint IS NULL OR user_id = $2)",
    )
    .bind(request_id)
    .bind(owner_user_id)
    .fetch_optional(pg)
    .await?;

    let Some(r) = row else {
        return Ok(None);
    };
    Ok(Some(ArchiveRecord {
        request_id: r.try_get("request_id")?,
        user_id: r.try_get("user_id")?,
        api_key_id: r.try_get("api_key_id")?,
        channel_id: r.try_get("channel_id")?,
        model: r.try_get("model")?,
        requested_model: r.try_get("requested_model")?,
        endpoint: r.try_get("endpoint")?,
        is_stream: r.try_get("is_stream")?,
        status_code: r.try_get("status_code")?,
        error_code: r.try_get("error_code")?,
        request_body: r.try_get("request_body")?,
        response_body: r.try_get("response_body")?,
        response_text: r.try_get("response_text")?,
        reasoning_text: r.try_get("reasoning_text")?,
        tool_calls: r.try_get("tool_calls")?,
        truncated: r.try_get("truncated")?,
        created_at: r.try_get("created_at")?,
        storage: "postgres".into(),
    }))
}

/// 一页存档摘要，连同实际读的是哪个后端（`"clickhouse"` / `"postgres"`）。
#[derive(Debug, Clone)]
pub struct ArchivePage {
    pub rows: Vec<ArchiveSummary>,
    pub backend: &'static str,
}

/// 单页上限与翻页深度上限。
const MAX_PAGE: u32 = 200;
const MAX_OFFSET: u32 = 10_000;

/// 分页检索对话存档列表。
///
/// ClickHouse 启用时读 ClickHouse；ClickHouse 写失败的那几条落在 PG（见 [`insert`]），这时两边按时间合并，
/// 详情与统计一直认它们，列表也要列得出来。只有 ClickHouse 不可用（[`StoreError::is_ch_unavailable`]）才整页
/// 改读 PG；查询本身出错照实报错，不伪装成「暂无数据」。
pub async fn list(
    pg: &PgPool,
    ch: Option<&ChClient>,
    filter: &ArchiveFilter,
) -> Result<ArchivePage, StoreError> {
    let limit = filter.limit.clamp(1, MAX_PAGE);
    let offset = filter.offset.min(MAX_OFFSET);
    if let Some(ch) = ch {
        let spilled = pg_has_rows(pg).await?;
        // 有落进 PG 的：两边各取到本页末尾为止，合并后再切页
        let (fetch, skip) = if spilled {
            (offset + limit, 0)
        } else {
            (limit, offset)
        };
        match list_ch(ch, filter, fetch, skip).await {
            Ok(rows) if !spilled => {
                return Ok(ArchivePage {
                    rows,
                    backend: "clickhouse",
                });
            }
            Ok(rows) => {
                let spill = list_pg(pg, filter, fetch, 0).await?;
                return Ok(ArchivePage {
                    rows: merge_newest_first(rows, spill, offset, limit),
                    backend: "clickhouse",
                });
            }
            Err(err) if err.is_ch_unavailable() => {
                tracing::warn!(error = %err, "ClickHouse 不可用，对话存档列表改查 PostgreSQL");
            }
            Err(err) => return Err(err),
        }
    }
    Ok(ArchivePage {
        rows: list_pg(pg, filter, limit, offset).await?,
        backend: "postgres",
    })
}

/// PG 里有没有存档（ClickHouse 启用时只有写失败降级的那几条）。
async fn pg_has_rows(pg: &PgPool) -> Result<bool, StoreError> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM conversation_archives)")
            .fetch_one(pg)
            .await?,
    )
}

/// 两段各自按时间倒序的摘要合并成一页；同一请求两边都有时留 ClickHouse 那条。
fn merge_newest_first(
    ch_rows: Vec<ArchiveSummary>,
    pg_rows: Vec<ArchiveSummary>,
    offset: u32,
    limit: u32,
) -> Vec<ArchiveSummary> {
    let seen: std::collections::HashSet<Uuid> = ch_rows.iter().map(|r| r.request_id).collect();
    let mut rows = ch_rows;
    rows.extend(
        pg_rows
            .into_iter()
            .filter(|r| !seen.contains(&r.request_id)),
    );
    rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
    rows.into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .collect()
}

async fn list_ch(
    ch: &ChClient,
    filter: &ArchiveFilter,
    limit: u32,
    offset: u32,
) -> Result<Vec<ArchiveSummary>, StoreError> {
    ensure_ch_table(ch).await?;
    let mut clauses = vec!["1 = 1".to_owned()];
    let mut owned_params: Vec<(&'static str, String)> = Vec::new();
    if let Some(uid) = filter.user_id {
        clauses.push("user_id = toUInt64({uid:String})".into());
        owned_params.push(("uid", uid.max(0).to_string()));
    }
    if let Some(kid) = filter.api_key_id {
        clauses.push("api_key_id = toUInt64({kid:String})".into());
        owned_params.push(("kid", kid.max(0).to_string()));
    }
    if let Some(model) = filter
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push("positionCaseInsensitiveUTF8(model, {model:String}) > 0".into());
        owned_params.push(("model", model.to_owned()));
    }
    if let Some(req_id) = filter.request_id {
        clauses.push("request_id = toUUID({req_id:String})".into());
        owned_params.push(("req_id", req_id.to_string()));
    }
    if let Some(q) = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push(
            "(positionCaseInsensitiveUTF8(response_text, {q:String}) > 0 \
              OR positionCaseInsensitiveUTF8(request_body, {q:String}) > 0)"
                .into(),
        );
        owned_params.push(("q", q.to_owned()));
    }
    if let Some(hours) = filter.hours {
        let clamped = hours.clamp(1, 24 * 365);
        clauses.push(format!("created_at >= now64(3) - INTERVAL {clamped} HOUR"));
    }

    let sql = format!(
        "SELECT toString(created_at) AS created_at_text, toString(request_id) AS request_id_text, \
         user_id, api_key_id, channel_id, model, requested_model, endpoint, is_stream, \
         status_code, error_code, truncated, \
         substringUTF8(request_body, 1, 600) AS req_snip, \
         substringUTF8(response_text, 1, 400) AS resp_snip \
         FROM conversation_archives FINAL \
         WHERE {} \
         ORDER BY created_at DESC \
         LIMIT {limit} OFFSET {offset}",
        clauses.join(" AND ")
    );
    let params: Vec<(&str, &str)> = owned_params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let rows = ch.query_with_params(&sql, &params).await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let req_id = Uuid::parse_str(&ch_str(row, "request_id_text")).ok()?;
            Some(ArchiveSummary {
                request_id: req_id,
                user_id: ch_i64(row, "user_id"),
                api_key_id: ch_i64(row, "api_key_id"),
                channel_id: ch_i64(row, "channel_id"),
                model: ch_str(row, "model"),
                requested_model: ch_str(row, "requested_model"),
                endpoint: ch_str(row, "endpoint"),
                is_stream: ch_i64(row, "is_stream") != 0,
                status_code: i16::try_from(ch_i64(row, "status_code")).unwrap_or(200),
                error_code: ch_str(row, "error_code"),
                request_preview: preview_text(&ch_str(row, "req_snip")),
                response_preview: preview_text(&ch_str(row, "resp_snip")),
                truncated: ch_i64(row, "truncated") != 0,
                created_at: parse_ch_ts(&ch_str(row, "created_at_text")),
                storage: "clickhouse".into(),
            })
        })
        .collect())
}

fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('%');
    for ch in raw.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

async fn list_pg(
    pg: &PgPool,
    filter: &ArchiveFilter,
    limit: u32,
    offset: u32,
) -> Result<Vec<ArchiveSummary>, StoreError> {
    let model_pat = filter
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(escape_like);
    let search_pat = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(escape_like);
    let since = filter
        .hours
        .map(|h| Utc::now() - Duration::hours(i64::from(h.clamp(1, 24 * 365))));
    let (limit, offset) = (i64::from(limit), i64::from(offset));

    let rows = sqlx::query(
        r"SELECT request_id, user_id, api_key_id, channel_id, model, requested_model,
                 endpoint, is_stream, status_code, error_code, truncated, created_at,
                 left(request_body, 600) AS req_snip,
                 left(response_text, 400) AS resp_snip
          FROM conversation_archives
          WHERE ($1::bigint IS NULL OR user_id = $1)
            AND ($2::bigint IS NULL OR api_key_id = $2)
            AND ($3::text IS NULL OR model ILIKE $3 ESCAPE '\')
            AND ($4::uuid IS NULL OR request_id = $4)
            AND ($5::text IS NULL OR response_text ILIKE $5 ESCAPE '\' OR request_body ILIKE $5 ESCAPE '\')
            AND ($6::timestamptz IS NULL OR created_at >= $6)
          ORDER BY created_at DESC
          LIMIT $7 OFFSET $8",
    )
    .bind(filter.user_id)
    .bind(filter.api_key_id)
    .bind(model_pat)
    .bind(filter.request_id)
    .bind(search_pat)
    .bind(since)
    .bind(limit)
    .bind(offset)
    .fetch_all(pg)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let req_snip: String = r.try_get("req_snip")?;
        let resp_snip: String = r.try_get("resp_snip")?;
        out.push(ArchiveSummary {
            request_id: r.try_get("request_id")?,
            user_id: r.try_get("user_id")?,
            api_key_id: r.try_get("api_key_id")?,
            channel_id: r.try_get("channel_id")?,
            model: r.try_get("model")?,
            requested_model: r.try_get("requested_model")?,
            endpoint: r.try_get("endpoint")?,
            is_stream: r.try_get("is_stream")?,
            status_code: r.try_get("status_code")?,
            error_code: r.try_get("error_code")?,
            request_preview: preview_text(&req_snip),
            response_preview: preview_text(&resp_snip),
            truncated: r.try_get("truncated")?,
            created_at: r.try_get("created_at")?,
            storage: "postgres".into(),
        });
    }
    Ok(out)
}

/// 删除单条存档（同时清理 PG 与 CH，满足合规删除需求）。
pub async fn delete_by_request_id(
    pg: &PgPool,
    ch: Option<&ChClient>,
    request_id: Uuid,
) -> Result<bool, StoreError> {
    let pg_res = sqlx::query("DELETE FROM conversation_archives WHERE request_id = $1")
        .bind(request_id)
        .execute(pg)
        .await?;
    let mut deleted = pg_res.rows_affected() > 0;
    if let Some(ch) = ch {
        ensure_ch_table(ch).await?;
        let sql = format!(
            "ALTER TABLE conversation_archives DELETE WHERE request_id = toUUID('{request_id}') SETTINGS mutations_sync = 1"
        );
        ch.execute(&sql).await?;
        deleted = true;
    }
    Ok(deleted)
}

/// 按保留天数定时清理过期存档：`retention_days == 0` 永久保留直接跳过。
pub async fn prune(
    pg: &PgPool,
    ch: Option<&ChClient>,
    retention_days: u32,
    now: DateTime<Utc>,
) -> Result<PruneReport, StoreError> {
    if retention_days == 0 {
        return Ok(PruneReport::default());
    }
    let cutoff = now - Duration::days(i64::from(retention_days));
    let mut pg_deleted = 0u64;
    for _ in 0..PG_PRUNE_MAX_ROUNDS {
        let res = sqlx::query(
            r"DELETE FROM conversation_archives
              WHERE request_id IN (
                  SELECT request_id FROM conversation_archives
                  WHERE created_at < $1
                  ORDER BY created_at ASC
                  LIMIT $2
              )",
        )
        .bind(cutoff)
        .bind(PG_PRUNE_BATCH)
        .execute(pg)
        .await?;
        let n = res.rows_affected();
        pg_deleted = pg_deleted.saturating_add(n);
        if n < u64::try_from(PG_PRUNE_BATCH).unwrap_or(u64::MAX) {
            break;
        }
    }

    let mut ch_deleted_requested = false;
    if let Some(ch) = ch {
        ensure_ch_table(ch).await?;
        let cutoff_str = fmt_ch_ts(cutoff);
        let sql = format!(
            "ALTER TABLE conversation_archives DELETE WHERE created_at < toDateTime64('{cutoff_str}', 3, 'UTC') SETTINGS mutations_sync = 0"
        );
        ch.execute(&sql).await?;
        ch_deleted_requested = true;
    }

    Ok(PruneReport {
        pg_deleted,
        ch_deleted_requested,
        cutoff: Some(cutoff),
    })
}

async fn ch_stats(ch: &ChClient) -> Result<Vec<Value>, StoreError> {
    ensure_ch_table(ch).await?;
    ch.query_json_each_row(
        "SELECT count() AS cnt, \
         if(count() > 0, toString(min(created_at)), '') AS oldest, \
         if(count() > 0, toString(max(created_at)), '') AS newest \
         FROM conversation_archives FINAL",
    )
    .await
}

/// 查询当前生效存储后端与存档数量/时间跨度。
pub async fn stats(pg: &PgPool, ch: Option<&ChClient>) -> Result<ArchiveStats, StoreError> {
    let pg_row = sqlx::query(
        r"SELECT COUNT(*)::bigint AS cnt, MIN(created_at) AS oldest, MAX(created_at) AS newest
          FROM conversation_archives",
    )
    .fetch_one(pg)
    .await?;
    let pg_cnt_i64: i64 = pg_row.try_get("cnt")?;
    let pg_count = u64::try_from(pg_cnt_i64.max(0)).unwrap_or(0);
    let mut oldest: Option<DateTime<Utc>> = pg_row.try_get("oldest")?;
    let mut newest: Option<DateTime<Utc>> = pg_row.try_get("newest")?;

    let mut ch_count = None;
    let ch_rows = match ch {
        Some(ch) => match ch_stats(ch).await {
            Ok(rows) => Some(rows),
            Err(err) if err.is_ch_unavailable() => {
                tracing::warn!(error = %err, "ClickHouse 不可用，对话存档统计只计 PostgreSQL");
                None
            }
            Err(err) => return Err(err),
        },
        None => None,
    };
    if let Some(row) = ch_rows.as_ref().and_then(|rows| rows.first()) {
        let c = u64::try_from(ch_i64(row, "cnt").max(0)).unwrap_or(0);
        ch_count = Some(c);
        if c > 0 {
            let ch_old = parse_ch_ts(&ch_str(row, "oldest"));
            let ch_new = parse_ch_ts(&ch_str(row, "newest"));
            oldest = Some(oldest.map_or(ch_old, |o| o.min(ch_old)));
            newest = Some(newest.map_or(ch_new, |n| n.max(ch_new)));
        }
    }

    let clickhouse_enabled = ch.is_some();
    let total_count = pg_count.saturating_add(ch_count.unwrap_or(0));
    Ok(ArchiveStats {
        // ClickHouse 不可用时新存档写进 PG：报实际在用的
        backend: if ch_count.is_some() {
            "clickhouse".into()
        } else {
            "postgres".into()
        },
        clickhouse_enabled,
        total_count,
        pg_count,
        ch_count,
        oldest_at: oldest,
        newest_at: newest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_pg_like_wildcards_and_truncates_previews() {
        assert_eq!(escape_like("gpt_4%\\x"), r"%gpt\_4\%\\x%");
        let long = "你好世界 ".repeat(60);
        let prev = preview_text(&long);
        assert!(prev.ends_with('…'));
        assert!(prev.chars().count() <= PREVIEW_CHARS + 1);
    }

    #[test]
    fn parses_clickhouse_timestamps() {
        let dt = parse_ch_ts("2026-11-22 10:15:30.456");
        assert_eq!(fmt_ch_ts(dt), "2026-11-22 10:15:30.456");
    }

    #[test]
    fn batch_dedup_token_keeps_single_compat_and_is_deterministic_for_batches() {
        let make = |id| ArchiveRecord {
            request_id: id,
            user_id: 1,
            api_key_id: 2,
            channel_id: 3,
            model: "m".into(),
            requested_model: "m".into(),
            endpoint: "/v1/chat/completions".into(),
            is_stream: false,
            status_code: 200,
            error_code: String::new(),
            request_body: String::new(),
            response_body: String::new(),
            response_text: String::new(),
            reasoning_text: String::new(),
            tool_calls: String::new(),
            truncated: false,
            created_at: Utc::now(),
            storage: String::new(),
        };
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let single = [make(a)];
        assert_eq!(batch_dedup_token(&single), format!("conv:{a}"));

        let batch1 = [make(a), make(b)];
        let batch2 = [make(a), make(b)];
        let batch3 = [make(b), make(a)];
        assert_eq!(batch_dedup_token(&batch1), batch_dedup_token(&batch2));
        assert_ne!(batch_dedup_token(&batch1), batch_dedup_token(&batch3));
        assert!(batch_dedup_token(&batch1).starts_with(&format!("conv-batch:{a}:2:")));
    }

    #[tokio::test]
    async fn ensure_ch_table_coalesces_concurrent_ddl_and_rechecks_after_invalidation() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let ddl_hits = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::clone(&ddl_hits);
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let hits = Arc::clone(&hits);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                });
            }
        });

        let ch = ChClient::new(&format!("http://{addr}"), "okapi").unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let ch = ch.clone();
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                ensure_ch_table(&ch).await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(
            ddl_hits.load(Ordering::SeqCst),
            1,
            "concurrent ensure_ch_table calls must singleflight into one DDL"
        );

        // Subsequent calls hit the lock-free fast path without touching ClickHouse.
        ensure_ch_table(&ch).await.unwrap();
        assert_eq!(ddl_hits.load(Ordering::SeqCst), 1);

        // After invalidation, another concurrent wave again issues exactly one DDL.
        ch.invalidate_archive_table();
        let barrier2 = Arc::new(tokio::sync::Barrier::new(4));
        let mut tasks2 = Vec::new();
        for _ in 0..4 {
            let ch = ch.clone();
            let barrier = Arc::clone(&barrier2);
            tasks2.push(tokio::spawn(async move {
                barrier.wait().await;
                ensure_ch_table(&ch).await
            }));
        }
        for task in tasks2 {
            task.await.unwrap().unwrap();
        }
        assert_eq!(ddl_hits.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
