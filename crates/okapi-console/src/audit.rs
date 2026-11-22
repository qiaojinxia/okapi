//! 审计日志读取与导出面（IMPLEMENTATION §11.15 + 审计增强）。
//!
//! 覆盖全部管理写操作、MCP 写工具与拒绝、用户安全/身份/密钥操作、团队/兑换/支付事件、
//! 后台 Worker 自动动作以及 `/admin/*` 越权拒绝留痕。
//!
//! 过滤参数全部走 sqlx 绑定参数（PG 侧转义），`action` 按前缀匹配（`user.` 拿到全部
//! 用户类动作），`ip` 支持精确地址或 CIDR 网段。列表按 id 倒序分页：页面用 `limit` / `offset` 并带回
//! 过滤后的 `total`（画页码）；`before`（id 游标）仍可用，翻页期间新写入的行不会造成重叠。

use super::query::Query;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okapi_api::permissions;
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    /// 操作者原文（`admin:42` / `user:7` / `mcp:3` / `worker` / `system:payment` / `anon`）。
    #[serde(default)]
    pub actor: Option<String>,
    /// 动作前缀（`channel.` / `user.login`）。
    #[serde(default)]
    pub action: Option<String>,
    /// 对象精确匹配（渠道 id / 用户 id / 分组码 / 邮箱）。
    #[serde(default)]
    pub target: Option<String>,
    /// 来源 IP 精确匹配或 CIDR 网段（如 `198.51.100.7` / `198.51.100.0/24`）。
    #[serde(default)]
    pub ip: Option<String>,
    /// 关键词检索（模糊匹配 target / actor / action / trace_id / user_agent / detail JSON）。
    #[serde(default)]
    pub q: Option<String>,
    /// W3C 链路 trace_id 精确匹配。
    #[serde(default)]
    pub trace_id: Option<String>,
    /// 操作凭证 API key ID 精确匹配（密钥泄露溯源）。
    #[serde(default)]
    pub actor_key_id: Option<i64>,
    /// 相对窗口小时数（缺省 168 = 7 天，上限 90 天）；给了 from 则忽略。
    #[serde(default)]
    pub hours: Option<u32>,
    #[serde(default)]
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    /// 游标：取该 id 之前的记录。
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
    /// 页码分页：跳过前 N 条（与 `before` 可叠加，页面只用这个）。
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct AuditExportQuery {
    #[serde(flatten)]
    pub query: AuditQuery,
    /// `csv`（默认）或 `json`。
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(sqlx::FromRow)]
struct AuditDbRow {
    id: i64,
    actor: String,
    action: String,
    target: Option<String>,
    detail: Option<Value>,
    ip: Option<String>,
    user_agent: Option<String>,
    trace_id: Option<String>,
    actor_key_id: Option<i64>,
    created_at: chrono::DateTime<chrono::Utc>,
}

struct NormalizedFilters {
    from: chrono::DateTime<chrono::Utc>,
    to: Option<chrono::DateTime<chrono::Utc>>,
    actor: Option<String>,
    action: Option<String>,
    target: Option<String>,
    ip: Option<String>,
    keyword: Option<String>,
    trace_id: Option<String>,
    actor_key_id: Option<i64>,
    before: Option<i64>,
}

fn trimmed(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn validate_ip_filter(raw: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(s) = trimmed(raw) else {
        return Ok(None);
    };
    if !okapi_store::netmatch::is_valid_entry(&s) {
        return Err(AppError::bad_request().with_param("ip"));
    }
    Ok(Some(s))
}

fn normalize_filters(q: &AuditQuery) -> Result<NormalizedFilters, AppError> {
    let hours = i64::from(q.hours.unwrap_or(168).clamp(1, 24 * 90));
    let from = q
        .from
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::hours(hours));
    let actor = trimmed(q.actor.as_deref());
    // 前缀匹配（`action LIKE $4 || '%'`）：动作名里的 `_` / `%` / `\` 按字面，不当通配
    let action =
        trimmed(q.action.as_deref()).map(|prefix| okapi_store::listing::escape_like(&prefix));
    let target = trimmed(q.target.as_deref());
    let ip = validate_ip_filter(q.ip.as_deref())?;
    let keyword = trimmed(q.q.as_deref()).map(|kw| {
        let capped: String = kw.chars().take(128).collect();
        okapi_store::listing::escape_like(&capped)
    });
    let trace_id = trimmed(q.trace_id.as_deref());
    Ok(NormalizedFilters {
        from,
        to: q.to,
        actor,
        action,
        target,
        ip,
        keyword,
        trace_id,
        actor_key_id: q.actor_key_id,
        before: q.before,
    })
}

/// 列表、计数与导出共用的过滤条件（$1–$10，绑定顺序见 [`bind_filters`]）。
macro_rules! audit_where {
    () => {
        r"
        WHERE created_at >= $1
          AND ($2::timestamptz IS NULL OR created_at < $2)
          AND ($3::text IS NULL OR actor = $3)
          AND ($4::text IS NULL OR action LIKE $4 || '%' ESCAPE '\')
          AND ($5::text IS NULL OR target = $5)
          AND ($6::bigint IS NULL OR id < $6)
          AND ($7::text IS NULL OR ip <<= $7::inet OR (ip IS NULL AND detail->>'ip' = $7))
          AND ($8::text IS NULL OR trace_id = $8)
          AND ($9::bigint IS NULL OR actor_key_id = $9)
          AND (
              $10::text IS NULL
              OR target ILIKE '%' || $10 || '%' ESCAPE '\'
              OR actor ILIKE '%' || $10 || '%' ESCAPE '\'
              OR action ILIKE '%' || $10 || '%' ESCAPE '\'
              OR COALESCE(trace_id, '') ILIKE '%' || $10 || '%' ESCAPE '\'
              OR COALESCE(user_agent, '') ILIKE '%' || $10 || '%' ESCAPE '\'
              OR COALESCE(detail::text, '') ILIKE '%' || $10 || '%' ESCAPE '\'
          )
        "
    };
}

fn bind_filters<'q, O>(
    query: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
    f: &'q NormalizedFilters,
) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments> {
    query
        .bind(f.from)
        .bind(f.to)
        .bind(&f.actor)
        .bind(&f.action)
        .bind(&f.target)
        .bind(f.before)
        .bind(&f.ip)
        .bind(&f.trace_id)
        .bind(f.actor_key_id)
        .bind(&f.keyword)
}

async fn fetch_audit_rows(
    state: &AppState,
    f: &NormalizedFilters,
    limit: i64,
    offset: i64,
) -> Result<Vec<AuditDbRow>, AppError> {
    let query = sqlx::query_as::<_, AuditDbRow>(concat!(
        r"
        SELECT id, actor, action, target, detail, host(ip) AS ip,
               user_agent, trace_id, actor_key_id, created_at
        FROM audit_logs",
        audit_where!(),
        r"
        ORDER BY id DESC
        LIMIT $11 OFFSET $12
        ",
    ));
    let rows = bind_filters(query, f)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(rows)
}

/// 过滤后的总条数（页码用）。审计表按月分区、按保留期清理，量级在万条以内。
async fn count_audit_rows(state: &AppState, f: &NormalizedFilters) -> Result<i64, AppError> {
    let query = sqlx::query_as::<_, (i64,)>(concat!(
        "SELECT COUNT(*)::bigint FROM audit_logs",
        audit_where!()
    ));
    let (total,) = bind_filters(query, f)
        .fetch_one(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(total)
}

/// 操作者标签回填：`admin:42` → 用户名；`mcp:3` → key 名 + 属主；`user:7` → 用户名；
/// `worker` / `system:*` 标注种类。原文保留在 `actor` 字段。
async fn resolve_actors(
    state: &AppState,
    actors: &[String],
) -> Result<HashMap<String, Value>, AppError> {
    let mut user_ids: Vec<i64> = Vec::new();
    let mut key_ids: Vec<i64> = Vec::new();
    for a in actors {
        if let Some((kind, id)) = a.split_once(':')
            && let Ok(id) = id.parse::<i64>()
        {
            match kind {
                "admin" | "user" => user_ids.push(id),
                "mcp" => key_ids.push(id),
                _ => {}
            }
        }
    }
    user_ids.sort_unstable();
    user_ids.dedup();
    key_ids.sort_unstable();
    key_ids.dedup();

    let users: HashMap<i64, String> = if user_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query!(
            r#"SELECT id, username FROM users WHERE id = ANY($1)"#,
            &user_ids
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .into_iter()
        .map(|r| (r.id, r.username))
        .collect()
    };
    let keys: HashMap<i64, (String, String)> = if key_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query!(
            r#"SELECT k.id, k.name, u.username FROM api_keys k JOIN users u ON u.id = k.user_id
               WHERE k.id = ANY($1)"#,
            &key_ids
        )
        .fetch_all(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .into_iter()
        .map(|r| (r.id, (r.name, r.username)))
        .collect()
    };

    Ok(actors
        .iter()
        .map(|a| {
            let (kind, id) = a.split_once(':').unwrap_or((a.as_str(), ""));
            let id_num = id.parse::<i64>().ok();
            let label = match kind {
                "admin" | "user" => id_num.and_then(|i| users.get(&i)).cloned(),
                "mcp" => id_num
                    .and_then(|i| keys.get(&i))
                    .map(|(name, owner)| format!("{owner} / {name}")),
                "system" if !id.is_empty() => Some(id.to_owned()),
                _ => None,
            };
            (
                a.clone(),
                json!({ "kind": kind, "id": id_num, "label": label }),
            )
        })
        .collect())
}

fn serialize_row(r: &AuditDbRow, labels: &HashMap<String, Value>) -> Value {
    json!({
        "id": r.id,
        "actor": r.actor,
        "actor_info": labels.get(&r.actor),
        "action": r.action,
        "target": r.target,
        "detail": r.detail,
        "ip": r.ip,
        "user_agent": r.user_agent,
        "trace_id": r.trace_id,
        "actor_key_id": r.actor_key_id,
        "created_at": r.created_at,
    })
}

/// GET /admin/audit：审计检索（倒序、页码 / 游标翻页、多维过滤），带过滤后的 `total`。
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::AUDIT_READ).await?;
    let limit = q
        .limit
        .unwrap_or(50)
        .clamp(1, okapi_store::listing::MAX_PAGE);
    let offset = q.offset.unwrap_or(0).max(0);
    let filters = normalize_filters(&q)?;
    let (rows, total) = tokio::try_join!(
        fetch_audit_rows(&state, &filters, limit, offset),
        count_audit_rows(&state, &filters),
    )?;

    let actors: Vec<String> = rows.iter().map(|r| r.actor.clone()).collect();
    let labels = resolve_actors(&state, &actors).await?;
    let shown = i64::try_from(rows.len()).unwrap_or(0);
    let has_more = offset.saturating_add(shown) < total;
    let next_before = rows.last().map(|r| r.id);
    let data: Vec<Value> = rows.iter().map(|r| serialize_row(r, &labels)).collect();
    Ok(Json(json!({
        "data": data,
        "total": total,
        "has_more": has_more,
        "next_before": if has_more { next_before } else { None },
    })))
}

/// CSV 单元格编码（RFC 4180 + 防电子表格公式注入）。
fn csv_cell(raw: &str) -> String {
    let mut s = raw.to_owned();
    let looks_like_formula = matches!(
        s.as_bytes().first(),
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r')
    );
    let looks_like_number = s.starts_with('-')
        && s.len() > 1
        && s[1..].bytes().all(|b| b.is_ascii_digit() || b == b'.');
    if looks_like_formula && !looks_like_number {
        s.insert(0, '\'');
    }
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn render_csv(rows: &[AuditDbRow], labels: &HashMap<String, Value>) -> String {
    let mut out = String::from(
        "\u{FEFF}id,created_at,actor,actor_label,action,target,ip,actor_key_id,trace_id,user_agent,detail\n",
    );
    for r in rows {
        let label = labels
            .get(&r.actor)
            .and_then(|v| v.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let ip =
            r.ip.as_deref()
                .or_else(|| {
                    r.detail
                        .as_ref()
                        .and_then(|d| d.get("ip"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("");
        let ua = r
            .user_agent
            .as_deref()
            .or_else(|| {
                r.detail
                    .as_ref()
                    .and_then(|d| d.get("ua"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
        let key_id = r.actor_key_id.map(|id| id.to_string()).unwrap_or_default();
        let detail_json = r.detail.as_ref().map(Value::to_string).unwrap_or_default();
        let _ = writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{}",
            r.id,
            csv_cell(&r.created_at.to_rfc3339()),
            csv_cell(&r.actor),
            csv_cell(label),
            csv_cell(&r.action),
            csv_cell(r.target.as_deref().unwrap_or("")),
            csv_cell(ip),
            key_id,
            csv_cell(r.trace_id.as_deref().unwrap_or("")),
            csv_cell(ua),
            csv_cell(&detail_json),
        );
    }
    out
}

/// GET /admin/audit/export：按筛选条件导出审计日志（`format=csv|json`，单次上限 5000 条），
/// 导出行为本身也记入 `audit.export` 审计。
pub async fn export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditExportQuery>,
) -> Result<Response, AppError> {
    let actor = super::admin::guard(&state, &headers, permissions::AUDIT_READ).await?;
    let format = q
        .format
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("csv")
        .to_ascii_lowercase();
    if !matches!(format.as_str(), "csv" | "json") {
        return Err(AppError::bad_request().with_param("format"));
    }
    let limit = q.query.limit.unwrap_or(2000).clamp(1, 5000);
    let filters = normalize_filters(&q.query)?;
    let mut rows = fetch_audit_rows(&state, &filters, limit + 1, 0).await?;
    let truncated = i64::try_from(rows.len()).unwrap_or(0) > limit;
    if truncated {
        rows.truncate(usize::try_from(limit).unwrap_or(2000));
    }
    let count = rows.len();
    let actors: Vec<String> = rows.iter().map(|r| r.actor.clone()).collect();
    let labels = resolve_actors(&state, &actors).await?;

    super::admin::audit(
        &state,
        &actor,
        "audit.export",
        &format,
        json!({
            "format": format,
            "count": count,
            "truncated": truncated,
            "actor_filter": filters.actor,
            "action_filter": q.query.action,
            "target_filter": filters.target,
            "ip_filter": filters.ip,
            "has_keyword": filters.keyword.is_some(),
        }),
    )
    .await;

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M");
    let filename = format!("audit-logs-{stamp}.{format}");
    let disposition = format!("attachment; filename=\"{filename}\"");
    let mut resp = if format == "json" {
        let data: Vec<Value> = rows.iter().map(|r| serialize_row(r, &labels)).collect();
        let body = serde_json::to_string_pretty(&json!({
            "exported_at": chrono::Utc::now(),
            "count": count,
            "truncated": truncated,
            "data": data,
        }))
        .map_err(|_| AppError::internal())?;
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body,
        )
            .into_response()
    } else {
        let body = render_csv(&rows, &labels);
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/csv; charset=utf-8")],
            body,
        )
            .into_response()
    };
    if let Ok(val) = axum::http::HeaderValue::from_str(&disposition) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, val);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store, private"),
    );
    if let Ok(val) = axum::http::HeaderValue::from_str(&count.to_string()) {
        resp.headers_mut()
            .insert(header::HeaderName::from_static("x-audit-export-count"), val);
    }
    resp.headers_mut().insert(
        header::HeaderName::from_static("x-audit-export-truncated"),
        axum::http::HeaderValue::from_static(if truncated { "true" } else { "false" }),
    );
    Ok(resp)
}

/// GET /admin/audit/actions：近 90 天出现过的动作名（过滤下拉的数据源）。
/// 动作名散落在各处理函数里，没有一份静态清单；从数据反推比维护常量表更不会漂移。
pub async fn actions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::AUDIT_READ).await?;
    let rows = sqlx::query_scalar!(
        r#"SELECT DISTINCT action FROM audit_logs
           WHERE created_at >= now() - interval '90 days' ORDER BY action"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({ "data": rows })))
}

/// `audit_logs.target` 的列宽（`varchar(128)`，按字符计）。
const AUDIT_TARGET_CHARS: usize = 128;

/// 登录审计的对象列。邮箱最长可到 254（注册校验的上限），失败登录更是任意长的串；原样写会超出列宽
/// （SQLSTATE 22001），这条审计——成功的或失败的——就丢了，而超长的撞库尝试恰恰最该留痕。
/// 截到列宽：记录比精确重要。
fn login_target(email: &str) -> String {
    email.chars().take(AUDIT_TARGET_CHARS).collect()
}

/// 统一审计写入入口：自动从当前 HTTP 请求上下文补充 `ip` / `user_agent` / `trace_id` / `actor_key_id`，
/// 标记请求已审计（防兜底重复写）；写失败只打日志不拖垮主流程。
pub(crate) async fn record_event(
    state: &AppState,
    actor: &str,
    action: &str,
    target: &str,
    detail: Value,
    actor_key_id: Option<i64>,
) {
    super::audit_fallback::note_audited();
    let meta = super::audit_fallback::current_meta(actor_key_id);
    record_event_with_meta(state, actor, action, target, detail, &meta).await;
}

/// 使用预捕获的请求上下文写入审计（供 `tokio::spawn` 等脱离原请求 task 的异步流程使用）。
pub(crate) async fn record_event_with_meta(
    state: &AppState,
    actor: &str,
    action: &str,
    target: &str,
    detail: Value,
    meta: &okapi_store::admin::AuditMeta,
) {
    if let Err(err) =
        okapi_store::admin::record_audit_with_meta(&state.pg, actor, action, target, detail, meta)
            .await
    {
        tracing::error!(error = %err, actor, action, "审计写入失败");
    }
}

/// 登录审计（成功 / 失败）。失败时按邮箱反查用户：真实用户要能在门户看到
/// "有人在试我的密码"，所以只要邮箱存在就记到该用户名下；邮箱不存在记 `anon`。
/// 写失败只打日志——审计不能反过来把登录拖垮。
pub async fn record_login(
    state: &AppState,
    email: &str,
    user_id: Option<i64>,
    ok: bool,
    reason: Option<&str>,
    ip: Option<String>,
    headers: &HeaderMap,
) {
    let resolved = if user_id.is_some() {
        user_id
    } else {
        sqlx::query_scalar!(
            r#"SELECT id FROM users WHERE email = $1 AND deleted_at IS NULL"#,
            email
        )
        .fetch_optional(&state.pg)
        .await
        .ok()
        .flatten()
    };
    let actor = resolved.map_or_else(|| "anon".to_owned(), |id| format!("user:{id}"));
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(200).collect::<String>());
    let action = if ok {
        "user.login"
    } else {
        "user.login_failed"
    };
    let mut meta = super::audit_fallback::current_meta(None);
    if let Some(parsed) = ip
        .as_deref()
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
    {
        meta.ip = Some(parsed);
    }
    if meta.user_agent.is_none() {
        meta.user_agent.clone_from(&ua);
    }
    let ip_detail = ip.or_else(|| meta.ip.map(|a| a.to_string()));
    record_event_with_meta(
        state,
        &actor,
        action,
        &login_target(email),
        json!({ "ip": ip_detail, "ua": ua, "reason": reason }),
        &meta,
    )
    .await;
}

#[derive(sqlx::FromRow)]
struct MyLoginRow {
    action: String,
    detail: Option<Value>,
    ip: Option<String>,
    user_agent: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// GET /api/me/logins：我最近的登录记录（含密码登录与 OAuth 登录，成功与失败各带 IP / UA / 时间）。
pub async fn my_logins(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = crate::console_auth::authenticate(&state, &headers).await?;
    let actor = format!("user:{}", key.user_id);
    let rows = sqlx::query_as::<_, MyLoginRow>(
        "SELECT action, detail, host(ip) AS ip, user_agent, created_at
         FROM audit_logs
         WHERE actor = $1 AND action IN ('user.login', 'user.login_failed')
         ORDER BY id DESC LIMIT 20",
    )
    .bind(actor)
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let detail = r.detail.unwrap_or(Value::Null);
            let ip =
                r.ip.map(Value::String)
                    .or_else(|| detail.get("ip").cloned())
                    .unwrap_or(Value::Null);
            let ua = r
                .user_agent
                .map(Value::String)
                .or_else(|| detail.get("ua").cloned())
                .unwrap_or(Value::Null);
            json!({
                "ok": r.action == "user.login",
                "at": r.created_at,
                "ip": ip,
                "ua": ua,
                "reason": detail.get("reason").cloned().unwrap_or(Value::Null),
                "method": detail.get("method").cloned().unwrap_or(Value::Null),
                "provider": detail.get("provider").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

#[cfg(test)]
mod tests {
    use super::{AUDIT_TARGET_CHARS, csv_cell, login_target, validate_ip_filter};

    #[test]
    fn a_login_target_never_exceeds_the_audit_column() {
        let valid_but_long = format!("{}@{}.example", "a".repeat(64), "b".repeat(180));
        assert!(valid_but_long.len() > AUDIT_TARGET_CHARS && valid_but_long.len() <= 254);
        for email in [
            valid_but_long,
            "x".repeat(10_000),
            "邮".repeat(300),
            "ünï".repeat(100),
        ] {
            let target = login_target(&email);
            assert_eq!(target.chars().count(), AUDIT_TARGET_CHARS);
            assert!(email.starts_with(&target), "只截尾部，不改动前面的内容");
        }
    }

    #[test]
    fn a_short_login_target_is_kept_as_is() {
        for email in ["", "a@b.co", &"y".repeat(AUDIT_TARGET_CHARS)] {
            assert_eq!(login_target(email), email);
        }
    }

    #[test]
    fn ip_filter_accepts_ip_and_cidr_and_rejects_garbage() {
        assert_eq!(validate_ip_filter(None).unwrap(), None);
        assert_eq!(validate_ip_filter(Some("   ")).unwrap(), None);
        assert_eq!(
            validate_ip_filter(Some("198.51.100.7")).unwrap().as_deref(),
            Some("198.51.100.7")
        );
        assert_eq!(
            validate_ip_filter(Some("198.51.100.0/24"))
                .unwrap()
                .as_deref(),
            Some("198.51.100.0/24")
        );
        assert_eq!(
            validate_ip_filter(Some("2001:db8::/32"))
                .unwrap()
                .as_deref(),
            Some("2001:db8::/32")
        );
        assert!(validate_ip_filter(Some("not-an-ip")).is_err());
        assert!(validate_ip_filter(Some("198.51.100.0/99")).is_err());
    }

    #[test]
    fn csv_cell_guards_against_formula_injection_and_quotes_specials() {
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell("-42.5"), "-42.5");
        assert_eq!(csv_cell("=CMD()"), "'=CMD()");
        assert_eq!(csv_cell("+1+1"), "'+1+1");
        assert_eq!(csv_cell("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(csv_cell("a,b\"c\nd"), "\"a,b\"\"c\nd\"");
    }
}
