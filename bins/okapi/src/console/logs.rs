//! 全站日志检索（IMPLEMENTATION §11.12）。
//!
//! 与 `stats.rs` 的分工：那边是**聚合看板**（读 MV，回答"整体怎么样"），
//! 这边是**逐笔明细**（读 `request_log_raw`，回答"这一笔到底怎么了"）。
//! 中转站日常最高频的工单是"我这次请求为什么失败/为什么这么贵"，
//! 只有明细能答——此前该能力仅存在于 MCP `search_logs`（且查的是 PG 而非 CH，
//! 没有渠道/TTFT/重试等排障必需列）与门户的"我自己的日志"。
//!
//! 与看板端点的另一处不同：这里的过滤条件含**用户输入的字符串**，
//! 故一律走 `query_with_params` 绑定参数，SQL 里只出现 clamp 过的整数。

use super::query::Query;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

fn ch_or_disabled(state: &AppState) -> Result<&ChClient, AppError> {
    state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))
}

fn ch_i64(row: &Value, key: &str) -> i64 {
    row.get(key).map_or(0, |v| {
        v.as_str()
            .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
            .unwrap_or(0)
    })
}

fn ch_str<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

// A reported zero is valid; a legacy zero cannot prove measurement took place.
fn observed_ms(row: &Value, value: &str, reported: &str) -> Option<i64> {
    let ms = ch_i64(row, value);
    let measured = if row[reported].is_null() {
        ms > 0
    } else {
        ch_i64(row, reported) == 1
    };
    (measured && ms >= 0).then_some(ms)
}

/// 检索条件。全部可选，叠加为 AND。
#[derive(Deserialize, Default)]
pub struct LogQuery {
    pub user_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub model: Option<String>,
    pub group: Option<String>,
    pub client_type: Option<String>,
    pub error_code: Option<String>,
    /// 按请求 ID 精确定位（工单里用户贴过来的就是它）。
    pub request_id: Option<String>,
    /// 上游请求 ID：给上游开工单时的反查锚点。
    pub upstream_request_id: Option<String>,
    /// 1充值 2消费 3管理 4系统 5错误 6退款 7登录（对齐 new-api 枚举）。
    pub log_type: Option<u8>,
    /// 只看失败：等价 `is_error = 1`，比让用户记住 log_type=5 友好。
    pub errors_only: Option<bool>,
    /// 回看小时数（1–2160，缺省 24）。给了 `from` 时忽略。
    pub hours: Option<u32>,
    /// 绝对区间（RFC3339；对账"某一天的账"用，new-api 日志页同有）。
    /// `from` 单独给 = 从该时刻到现在；`to` 为开区间上界。
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// RFC3339 → CH DateTime64 参数字面量（UTC，秒精度足够——日志页不按毫秒对账）。
fn ch_datetime(input: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(input.trim())
        .ok()
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
}

/// 编译后的检索条件：整数已 clamp 进 SQL，字符串留在绑定参数里。
struct Filters {
    clause: String,
    params: Vec<(String, String)>,
}

impl LogQuery {
    fn hours(&self) -> u32 {
        self.hours.unwrap_or(24).clamp(1, 2160)
    }
    fn limit(&self) -> u32 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
    /// 翻页上限刻意压在 1 万：再往后翻不是人的用法，是把明细当导出用；
    /// 需要全量导出应走 CH 直连而不是让 console 扛深翻页。
    fn offset(&self) -> u32 {
        self.offset.unwrap_or(0).min(10_000)
    }

    /// 时间窗是绝对区间还是相对小时数：给了 `from` 就是绝对区间。
    /// 给了却解析不了 → 400 而非静默回落相对窗口——管理员要"8 月 30 日的账"
    /// 却拿到"最近 24 小时"，比报错糟得多。
    fn absolute_range(&self) -> Result<Option<(String, Option<String>)>, AppError> {
        let Some(from_raw) = self
            .from
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(None);
        };
        let from =
            ch_datetime(from_raw).ok_or_else(|| AppError::bad_request().with_param("from"))?;
        let to = match self.to.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => {
                Some(ch_datetime(raw).ok_or_else(|| AppError::bad_request().with_param("to"))?)
            }
            None => None,
        };
        Ok(Some((from, to)))
    }

    fn filters(&self) -> Result<Filters, AppError> {
        use std::fmt::Write as _;

        let mut params: Vec<(String, String)> = Vec::new();
        // 时间边界同样走绑定参数，不拼进 SQL
        let mut clause = match self.absolute_range()? {
            Some((from, to)) => {
                params.push(("p_from".to_owned(), from));
                let mut c = "ts >= {p_from:DateTime64(3, 'UTC')}".to_owned();
                if let Some(to) = to {
                    params.push(("p_to".to_owned(), to));
                    c.push_str(" AND ts < {p_to:DateTime64(3, 'UTC')}");
                }
                c
            }
            None => format!("ts >= now() - INTERVAL {} HOUR", self.hours()),
        };

        let mut num = |col: &str, value: Option<i64>| {
            if let Some(v) = value {
                let _ = write!(clause, " AND {col} = {v}");
            }
        };
        num("user_id", self.user_id);
        num("api_key_id", self.api_key_id);
        num("channel_id", self.channel_id);
        if let Some(t) = self.log_type {
            let _ = write!(clause, " AND log_type = {t}");
        }
        if self.errors_only == Some(true) {
            clause.push_str(" AND is_error = 1");
        }

        let mut text = |col: &str, name: &str, value: Option<&String>| {
            if let Some(v) = value.map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let _ = write!(clause, " AND {col} = {{{name}:String}}");
                params.push((name.to_owned(), v.to_owned()));
            }
        };
        text("model", "p_model", self.model.as_ref());
        text("group_code", "p_group", self.group.as_ref());
        text("client_type", "p_client", self.client_type.as_ref());
        text("error_code", "p_error", self.error_code.as_ref());
        text(
            "upstream_request_id",
            "p_ureq",
            self.upstream_request_id.as_ref(),
        );
        // request_id 是 UUID 列，绑定参数按 UUID 类型解析——传入非法 UUID 时
        // 由 CH 报错而非静默全表扫，正是我们要的 fail-fast。
        if let Some(v) = self
            .request_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            clause.push_str(" AND request_id = {p_reqid:UUID}");
            params.push(("p_reqid".to_owned(), v.to_owned()));
        }

        Ok(Filters { clause, params })
    }
}

fn borrow(params: &[(String, String)]) -> Vec<(&str, &str)> {
    params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

async fn billing_states(
    state: &AppState,
    rows: &[Value],
) -> Result<HashMap<uuid::Uuid, (i64, i16)>, AppError> {
    let ids: Vec<uuid::Uuid> = rows
        .iter()
        .filter_map(|row| ch_str(row, "request_id").parse().ok())
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let states = sqlx::query_as::<_, (uuid::Uuid, i64, i16)>(
        "SELECT request_id,user_id,status FROM billing_financial_records WHERE request_id=ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(states
        .into_iter()
        .map(|(request, user, status)| (request, (user, status)))
        .collect())
}

fn billing_status(row: &Value, states: &HashMap<uuid::Uuid, (i64, i16)>) -> Value {
    ch_str(row, "request_id")
        .parse()
        .ok()
        .and_then(|id| states.get(&id))
        .filter(|(owner, _)| *owner == ch_i64(row, "user_id"))
        .map_or_else(
            || row["billing_status"].clone(),
            |(_, status)| json!(status),
        )
}

/// GET /admin/logs：全站逐笔明细检索。
///
/// 权限沿用 `billing.read`：每一行都带金额与倍率快照，单独立一个 `logs.read`
/// 只会造出"能看日志但不能看钱"的空档位——而这里没有一行是不含钱的
/// （与 §11.6"统计不另立 stats.read"同一判据）。
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let filters = q.filters()?;

    // 不给 ts/request_id 起 toString 别名：ClickHouse 的 WHERE 会优先解析 SELECT
    // 别名，`toString(ts) AS ts` 会让时间过滤变成 String 与 DateTime 比较而报错；
    // JSONEachRow 本就把 DateTime64/UUID 序列化为字符串，无需手动转。
    let sql = format!(
        "SELECT ts, request_id, upstream_request_id, \
                log_type, user_id, api_key_id, group_code, model, channel_id, channel_key_id, \
                client_type, client_ip, node, \
                prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens, input_unit, input_characters, \
                prompt_source, completion_source, upstream_prompt_tokens, upstream_completion_tokens, \
                requested_model, upstream_model, endpoint, upstream_endpoint, \
                cache_write_tokens, cache_read_reported, cache_write_reported, cache_write_5m_tokens, cache_write_1h_tokens, \
                audio_prompt_tokens, image_prompt_tokens, audio_completion_tokens, image_completion_tokens, \
                cache_read_audio_tokens, cache_read_image_tokens, cache_write_audio_tokens, cache_write_image_tokens, \
                audio_prompt_reported, image_prompt_reported, audio_completion_reported, image_completion_reported, \
                cache_read_audio_reported, cache_read_image_reported, cache_write_audio_reported, cache_write_image_reported, reasoning_reported, \
                amount_micro, original_amount_micro, discount_micro, upstream_cost_micro, \
                latency_ms, latency_reported, ttft_ms, ttft_reported, stream, request_type, retry_count, failover_count, sticky_layer, \
                upstream_status, error_code, is_error, ratio_snapshot, diagnostics, upstream_cost_known, billing_status \
         FROM request_log_raw WHERE {} \
         ORDER BY ts DESC LIMIT {} OFFSET {}",
        filters.clause,
        q.limit(),
        q.offset(),
    );
    let rows = ch.query_with_params(&sql, &borrow(&filters.params)).await?;

    // id → 名字回填（用户/密钥/渠道），筛选对象即使没有命中行也要能识别。
    // 与 stats::channels 同法——存 id、查询时 join，改名不脏历史。
    let names = resolve_names(&state, &rows, &q).await;

    // Refunds can change the ledger state after the immutable CH row was delivered.
    let financial_states = billing_states(&state, &rows).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let user_id = ch_i64(r, "user_id");
            let api_key_id = ch_i64(r, "api_key_id");
            let key = names.keys.get(&api_key_id);
            let channel_id = ch_i64(r, "channel_id");
            let usage = log_usage(r);
            let mut item = json!({
                "ts": ch_str(r, "ts"),
                "request_id": ch_str(r, "request_id"),
                "upstream_request_id": ch_str(r, "upstream_request_id"),
                "log_type": ch_i64(r, "log_type"),
                "user_id": user_id,
                "username": names.users.get(&user_id).cloned().unwrap_or_default(),
                "api_key_id": api_key_id,
                "key_name": key.map(|(name, _, _)| name.as_str()).unwrap_or_default(),
                "key_prefix": key.map(|(_, prefix, _)| prefix.as_str()).unwrap_or_default(),
                "group": ch_str(r, "group_code"),
                "model": ch_str(r, "model"),
                "requested_model": ch_str(r, "requested_model"),
                "upstream_model": ch_str(r, "upstream_model"),
                "endpoint": ch_str(r, "endpoint"),
                "upstream_endpoint": ch_str(r, "upstream_endpoint"),
                "usage_details_recorded": !r["cache_write_tokens"].is_null(),
                "channel_id": channel_id,
                "channel_name": names.channel_name(channel_id),
                "channel_key_id": ch_i64(r, "channel_key_id"),
                // provider 取 PG 渠道行而非 CH 列：chsink 的 `build_ch_row` 把该列
                // 硬编码为空串，读它只会给排障者一个永远空白的字段。
                "provider": names.provider(channel_id),
                "client_type": ch_str(r, "client_type"),
                "client_ip": ch_str(r, "client_ip"),
                "node": ch_str(r, "node"),
                "usage": usage,
                "amount_micro": ch_i64(r, "amount_micro"),
                "original_amount_micro": ch_i64(r, "original_amount_micro"),
                "discount_micro": ch_i64(r, "discount_micro"),
                // 上游成本（§11.18）：管理面账单解释器展示毛利；门户接口不透出此字段
                "upstream_cost_micro": ch_i64(r, "upstream_cost_micro"),
                "latency_ms": observed_ms(r, "latency_ms", "latency_reported"),
                "ttft_ms": observed_ms(r, "ttft_ms", "ttft_reported"),
                "is_stream": ch_i64(r, "stream") == 1,
                "retry_count": ch_i64(r, "retry_count"),
                "failover_count": ch_i64(r, "failover_count"),
                "sticky_layer": ch_i64(r, "sticky_layer"),
                "upstream_status": ch_i64(r, "upstream_status"),
                "error_code": ch_str(r, "error_code"),
                "is_error": ch_i64(r, "is_error") == 1,
                "ratio_snapshot": ch_str(r, "ratio_snapshot"),
            });
            item["request_type"] = json!(ch_str(r, "request_type"));
            item["status"] = billing_status(r, &financial_states);
            item["upstream_cost_known"] = json!(r["upstream_cost_known"].as_u64().map(|v| v == 1));
            item["diagnostics"] =
                serde_json::from_str::<Value>(ch_str(r, "diagnostics")).unwrap_or(Value::Null);
            item
        })
        .collect();

    Ok(Json(json!({
        "scope": names.scope(&q),
        "hours": q.hours(),
        "from": q.from,
        "to": q.to,
        "limit": q.limit(),
        "offset": q.offset(),
        "data": data,
    })))
}

fn log_usage(r: &Value) -> Value {
    json!({
        "reported_details": super::usage_observations::from_ch(r),
        "prompt_tokens": ch_i64(r, "prompt_tokens"),
        "input_unit": r["input_unit"],
        "input_characters": r["input_characters"],
        "cached_tokens": ch_i64(r, "cached_tokens"),
        "completion_tokens": ch_i64(r, "completion_tokens"),
        "reasoning_tokens": ch_i64(r, "reasoning_tokens"),
        "cache_write_tokens": r["cache_write_tokens"],
        "cache_read_reported": r["cache_read_reported"].as_u64().map(|v| v == 1),
        "cache_write_reported": r["cache_write_reported"].as_u64().map(|v| v == 1),
        "cache_write_5m_tokens": r["cache_write_5m_tokens"],
        "cache_write_1h_tokens": r["cache_write_1h_tokens"],
        "audio_prompt_tokens": r["audio_prompt_tokens"],
        "image_prompt_tokens": r["image_prompt_tokens"],
        "audio_completion_tokens": r["audio_completion_tokens"],
        "image_completion_tokens": r["image_completion_tokens"],
        "cache_read_modalities": if r["cache_read_audio_tokens"].is_null() || r["cache_read_image_tokens"].is_null() { Value::Null } else { json!({"audio_tokens":r["cache_read_audio_tokens"],"image_tokens":r["cache_read_image_tokens"]}) },
        "cache_write_modalities": if r["cache_write_audio_tokens"].is_null() || r["cache_write_image_tokens"].is_null() { Value::Null } else { json!({"audio_tokens":r["cache_write_audio_tokens"],"image_tokens":r["cache_write_image_tokens"]}) },
        "prompt_source": ch_str(r, "prompt_source"),
        "completion_source": ch_str(r, "completion_source"),
        "upstream_usage": if ch_str(r, "prompt_source") == "unknown" && ch_str(r, "completion_source") == "unknown" {
            Value::Null
        } else { json!({"prompt_tokens":r["upstream_prompt_tokens"], "completion_tokens":r["upstream_completion_tokens"]}) },
    })
}

#[derive(Default)]
struct Names {
    users: HashMap<i64, String>,
    /// 仅返回名称、非敏感前缀与属主 ID；不读取 key_hash 或凭证。
    keys: HashMap<i64, (String, String, i64)>,
    channels: HashMap<i64, (String, String)>,
}

impl Names {
    fn scope(&self, q: &LogQuery) -> Value {
        let mut scope = serde_json::Map::new();
        if let Some(id) = q.user_id {
            scope.insert(
                "user".into(),
                json!({ "id": id, "username": self.users.get(&id) }),
            );
        }
        if let Some(id) = q.api_key_id {
            let key = self.keys.get(&id);
            scope.insert(
                "api_key".into(),
                json!({
                    "id": id,
                    "name": key.map(|(name, _, _)| name),
                    "key_prefix": key.map(|(_, prefix, _)| prefix),
                    "user_id": key.map(|(_, _, owner)| owner),
                    "username": key.and_then(|(_, _, owner)| self.users.get(owner)),
                }),
            );
        }
        if let Some(id) = q.channel_id {
            let channel = self.channels.get(&id);
            scope.insert(
                "channel".into(),
                json!({
                    "id": id,
                    "name": channel.map(|(name, _)| name),
                    "provider": channel.map(|(_, provider)| provider),
                }),
            );
        }
        Value::Object(scope)
    }

    fn channel_name(&self, id: i64) -> String {
        self.channels
            .get(&id)
            .map(|(n, _)| n.clone())
            .unwrap_or_default()
    }
    fn provider(&self, id: i64) -> String {
        self.channels
            .get(&id)
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    }
}

/// 有界 PG 批量点查：当前页与已选 ID 一起解析，空结果也返回筛选名称。
async fn resolve_names(state: &AppState, rows: &[Value], q: &LogQuery) -> Names {
    let mut key_ids: Vec<i64> = rows
        .iter()
        .map(|r| ch_i64(r, "api_key_id"))
        .chain(q.api_key_id)
        .filter(|id| *id > 0)
        .collect();
    key_ids.sort_unstable();
    key_ids.dedup();
    let mut names = Names::default();
    if !key_ids.is_empty()
        && let Ok(rows) = sqlx::query!(
            r#"SELECT id, name, key_prefix, user_id FROM api_keys WHERE id = ANY($1)"#,
            &key_ids
        )
        .fetch_all(&state.pg)
        .await
    {
        names.keys = rows
            .into_iter()
            .map(|r| (r.id, (r.name, r.key_prefix, r.user_id)))
            .collect();
    }
    let mut user_ids: Vec<i64> = rows
        .iter()
        .map(|r| ch_i64(r, "user_id"))
        .chain(q.user_id)
        .chain(names.keys.values().map(|(_, _, owner)| *owner))
        .filter(|id| *id > 0)
        .collect();
    user_ids.sort_unstable();
    user_ids.dedup();
    let mut channel_ids: Vec<i64> = rows
        .iter()
        .map(|r| ch_i64(r, "channel_id"))
        .chain(q.channel_id)
        .filter(|id| *id > 0)
        .collect();
    channel_ids.sort_unstable();
    channel_ids.dedup();

    if !user_ids.is_empty()
        && let Ok(rows) = sqlx::query!(
            r#"SELECT id, username FROM users WHERE id = ANY($1)"#,
            &user_ids
        )
        .fetch_all(&state.pg)
        .await
    {
        names.users = rows.into_iter().map(|r| (r.id, r.username)).collect();
    }
    if !channel_ids.is_empty()
        && let Ok(rows) = sqlx::query!(
            r#"SELECT id, name, provider FROM channels WHERE id = ANY($1)"#,
            &channel_ids
        )
        .fetch_all(&state.pg)
        .await
    {
        names.channels = rows
            .into_iter()
            .map(|r| (r.id, (r.name, r.provider)))
            .collect();
    }
    names
}

/// GET /admin/logs/stat：日志页统计条（docs/database.md §3.5 末条）。
///
/// 语义定案照设计走：**无过滤条件时 RPM/TPM 取 Redis 秒桶**（秒级新鲜，
/// 且完全不碰 CH）；**带过滤时退化为 CH 最近 60s 窗口**——过滤维度组合无穷，
/// 不可能为每种组合维护 Redis 计数器。窗口累计（消耗/请求/错误）恒走 CH，
/// 因为它要跨小时甚至跨天，本就不是秒桶能表达的。
pub async fn stat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogQuery>,
) -> Result<Json<Value>, AppError> {
    super::admin::guard(&state, &headers, permissions::BILLING_READ).await?;
    let ch = ch_or_disabled(&state)?;
    let filters = q.filters()?;
    let params = borrow(&filters.params);

    // 聚合别名一律不与原始列同名（sum(prompt_tokens) AS prompt_tokens 会让
    // 同查询里其它聚合的 prompt_tokens 解析到别名上，报"聚合套聚合"）。
    let provenance_sql = super::usage_sources::raw_sql();
    let observations_sql = super::usage_observations::ch_sql();
    let units_sql = super::input_units::raw_sql();
    let rate_sql = super::output_rate::raw_sql();
    let sql = format!(
        "SELECT {provenance_sql}, {observations_sql}, {units_sql}, {rate_sql}, count() AS requests, sum(is_error) AS errors, \
                sum(prompt_tokens + completion_tokens) AS tokens, \
                sum(cached_tokens) AS cached, sum(prompt_tokens) AS prompt, \
                countIf(ifNull(cache_read_reported, 0) = 1) AS cache_read_known, \
                sumOrNullIf(cache_write_tokens, ifNull(cache_write_reported, 0) = 1 OR cache_write_tokens > 0) AS cache_write_total, \
                countIf(ifNull(cache_write_reported, 0) = 1 OR cache_write_tokens > 0) AS cache_write_samples, \
                sumOrNull(cache_write_5m_tokens) AS cache_write_5m_total, sumOrNull(cache_write_1h_tokens) AS cache_write_1h_total, \
                countIf(isNotNull(cache_write_5m_tokens) AND isNotNull(cache_write_1h_tokens)) AS cache_write_ttl_samples, \
                sum(reasoning_tokens) AS reasoning_total, \
                sumOrNull(audio_prompt_tokens) AS audio_prompt_total, count(audio_prompt_tokens) AS audio_prompt_samples, \
                sumOrNull(image_prompt_tokens) AS image_prompt_total, count(image_prompt_tokens) AS image_prompt_samples, \
                sumOrNull(audio_completion_tokens) AS audio_completion_total, count(audio_completion_tokens) AS audio_completion_samples, \
                sumOrNull(image_completion_tokens) AS image_completion_total, count(image_completion_tokens) AS image_completion_samples, \
                sumOrNull(cache_read_audio_tokens) AS cache_read_audio_total, sumOrNull(cache_read_image_tokens) AS cache_read_image_total, \
                sumOrNull(cache_write_audio_tokens) AS cache_write_audio_total, sumOrNull(cache_write_image_tokens) AS cache_write_image_total, \
                count(cache_read_audio_tokens) AS cache_read_modal_samples, count(cache_write_audio_tokens) AS cache_write_modal_samples, \
                sum(amount_micro) AS amount, sum(discount_micro) AS saved, \
                uniqExact(user_id) AS users \
         FROM request_log_calls WHERE {}",
        filters.clause
    );
    let finance_sql = format!(
        "SELECT count() AS financial_records,countIf(log_type NOT IN (2,5,6)) AS unclassified_records,sum(amount_micro) AS amount,sum(discount_micro) AS saved FROM request_log_raw WHERE {}",
        filters.clause
    );
    let (mut rows, financial) = tokio::try_join!(
        ch.query_with_params(&sql, &params),
        ch.query_with_params(&finance_sql, &params)
    )?;
    ensure_classified_summary(&financial)?;
    if let (Some(row), Some(finance)) = (rows.first_mut(), financial.first()) {
        for field in ["financial_records", "amount", "saved"] {
            row[field] = finance[field].clone();
        }
    }
    let row = rows.first().cloned().unwrap_or(Value::Null);

    let filtered = !filters.params.is_empty()
        || q.user_id.is_some()
        || q.api_key_id.is_some()
        || q.channel_id.is_some()
        || q.log_type.is_some()
        || q.errors_only == Some(true);
    let (rpm, tpm, source) = if filtered {
        let sql = format!(
            "SELECT count() AS rpm, sum(prompt_tokens + completion_tokens) AS tpm \
             FROM request_log_calls WHERE {} AND ts >= now() - INTERVAL 60 SECOND",
            filters.clause
        );
        let recent = ch.query_with_params(&sql, &params).await?;
        let recent = recent.first().cloned().unwrap_or(Value::Null);
        (ch_i64(&recent, "rpm"), ch_i64(&recent, "tpm"), "clickhouse")
    } else {
        let window = state.sched.kpi_window(60).await;
        (
            window.iter().map(|s| s.requests).sum(),
            window.iter().map(|s| s.tokens).sum(),
            "redis",
        )
    };

    let requests = ch_i64(&row, "requests");
    let prompt = ch_i64(&row, "prompt");
    let cached = ch_i64(&row, "cached");
    let mut result = json!({
        "hours": q.hours(),
        "financial_records": ch_i64(&row, "financial_records"),
        "requests": requests,
        "errors": ch_i64(&row, "errors"),
        "error_rate_bp": rate_bp(ch_i64(&row, "errors"), requests),
        "tokens": ch_i64(&row, "tokens"),
        "amount_micro": ch_i64(&row, "amount"),
        "discount_micro": ch_i64(&row, "saved"),
        "users": ch_i64(&row, "users"),
        // 缓存命中口径 = 命中 token / 输入 token。按"请求是否命中"计会高估收益：
        // 一次只命中 5% 前缀的请求和一次全命中的请求，省下的钱差两个数量级。
        "cached_tokens": cached,
        "cache_hit_bp": super::usage_details::cache_rate(cached, prompt, requests, ch_i64(&row, "cache_read_known")),
        "cache_read_known_requests": ch_i64(&row, "cache_read_known"),
        "rpm": rpm,
        "tpm": tpm,
        "rate_source": source,
    });
    add_summary_details(&row, &mut result);
    super::usage_observations::enrich(&row, &mut result, requests);
    if let Some(object) = result.as_object_mut() {
        object.extend(super::input_units::metrics(&row, requests));
        object.extend(super::output_rate::metrics(&row, requests));
    }
    Ok(Json(result))
}

fn ensure_classified_summary(financial: &[Value]) -> Result<(), AppError> {
    if financial
        .first()
        .is_some_and(|row| ch_i64(row, "unclassified_records") > 0)
    {
        return Err(
            okapi_store::StoreError::InvalidData("statistics_request_history_incomplete").into(),
        );
    }
    Ok(())
}

fn add_summary_details(row: &Value, result: &mut Value) {
    let requests = ch_i64(row, "requests");
    let prompt = ch_i64(row, "prompt");
    let cached = ch_i64(row, "cached");
    for (name, value) in super::usage_sources::metrics(
        row,
        requests,
        [Some(prompt), Some(ch_i64(row, "source_completion_total"))],
        Some(cached),
        ch_i64(row, "cache_read_known"),
    ) {
        result[name] = value;
    }
    result["records"] = json!(requests);
    for (name, column) in [
        ("cache_write_tokens", "cache_write_total"),
        ("cache_write_5m_tokens", "cache_write_5m_total"),
        ("cache_write_1h_tokens", "cache_write_1h_total"),
        ("reasoning_tokens", "reasoning_total"),
        ("audio_prompt_tokens", "audio_prompt_total"),
        ("image_prompt_tokens", "image_prompt_total"),
        ("audio_completion_tokens", "audio_completion_total"),
        ("image_completion_tokens", "image_completion_total"),
        ("cache_read_audio_tokens", "cache_read_audio_total"),
        ("cache_read_image_tokens", "cache_read_image_total"),
        ("cache_write_audio_tokens", "cache_write_audio_total"),
        ("cache_write_image_tokens", "cache_write_image_total"),
    ] {
        result[name] = if row[column].is_null() {
            Value::Null
        } else {
            json!(ch_i64(row, column))
        };
    }
    for name in [
        "cache_write_samples",
        "cache_write_ttl_samples",
        "audio_prompt_samples",
        "image_prompt_samples",
        "audio_completion_samples",
        "image_completion_samples",
        "cache_read_modal_samples",
        "cache_write_modal_samples",
    ] {
        result[name] = json!(ch_i64(row, name));
    }
}

/// 占比 → 基点（万分之一；整数运算，分母 0 返 0）。
fn rate_bp(part: i64, total: i64) -> i64 {
    super::stats::rate_bp(part, total)
}

#[cfg(test)]
mod timing_tests {
    use super::observed_ms;
    use serde_json::json;

    #[test]
    fn missing_legacy_timing_is_not_a_reported_zero() {
        for (row, expected) in [
            (json!({"ms":0,"reported":1}), Some(0)),
            (json!({"ms":"15","reported":"1"}), Some(15)),
            (json!({"ms":100,"reported":0}), None),
            (json!({"ms":-1,"reported":1}), None),
            (json!({"ms":0}), None),
            (json!({"ms":15}), Some(15)),
        ] {
            assert_eq!(observed_ms(&row, "ms", "reported"), expected, "{row}");
        }
    }
}
