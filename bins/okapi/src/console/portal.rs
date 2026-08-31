//! 用户门户 API（M2 第一批）：余额 / 用量 / key 分账。
//!
//! 合作商轻量模式（IMPLEMENTATION §6.1 的 key 即子账户）：给每位员工发独立 key，
//! `/api/me/usage` 默认 `scope=key`（员工只看自己这把 key 的用量），
//! `scope=user` 为钱包主体汇总视图。完整 Team 层（独立登录/成员限额）在 M4。
//! 统计查询走 ClickHouse MV；未启用 CH 时 fail-closed 返回 501 stats_disabled。

use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};

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

/// GET /api/me：身份与余额（热余额为准，快照列对账用）。
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let balance = state.ledger.balance(key.user_id).await?;
    Ok(Json(json!({
        "user_id": key.user_id,
        "key_id": key.key_id,
        "group": key.group_code,
        "balance_micro": balance.as_micros(),
    })))
}

#[derive(Deserialize)]
pub struct UsageQuery {
    #[serde(default = "default_days")]
    pub days: u16,
    /// key（默认：当前 key 视角，员工子账户语义）| user（钱包主体汇总）。
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_days() -> u16 {
    7
}

fn default_scope() -> String {
    "key".to_owned()
}

/// GET /api/me/usage：按天用量（CH MV，聚合与请求量解耦）。
pub async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let ch = ch_or_disabled(&state)?;
    let days = q.days.clamp(1, 90);

    let sql = match q.scope.as_str() {
        "user" => format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
                    sumMerge(amount) AS amount_micro \
             FROM mv_user_day WHERE user_id = {} AND day >= today() - {days} \
             GROUP BY day ORDER BY day",
            key.user_id
        ),
        _ => format!(
            "SELECT day, countMerge(requests) AS requests, sumMerge(tokens) AS tokens, \
                    sumMerge(amount) AS amount_micro \
             FROM mv_apikey_day WHERE api_key_id = {} AND day >= today() - {days} \
             GROUP BY day ORDER BY day",
            key.key_id
        ),
    };
    let rows = ch.query_json_each_row(&sql).await.map_err(AppError::from)?;
    let total: i64 = rows.iter().map(|r| ch_i64(r, "amount_micro")).sum();
    Ok(Json(json!({
        "scope": if q.scope == "user" { "user" } else { "key" },
        "days": days,
        "total_amount_micro": total,
        "data": rows,
    })))
}

/// GET /api/pricing：公开价格页（无鉴权）——模型倍率/按次价 + 分组倍率。
/// 只暴露定价事实，不含渠道/成本信息。
pub async fn public_pricing(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let models = sqlx::query!(
        r#"SELECT m.model_name, m.display_name, m.vendor, p.pricing_mode,
                  p.model_ratio::text AS model_ratio,
                  p.completion_ratio::text AS completion_ratio,
                  p.cache_ratio::text AS cache_ratio,
                  p.cache_write_ratio::text AS cache_write_ratio,
                  p.audio_ratio::text AS audio_ratio,
                  p.audio_completion_ratio::text AS audio_completion_ratio,
                  p.image_ratio::text AS image_ratio,
                  p.per_call_price_micro
           FROM models m JOIN model_pricing p ON p.model_id = m.id
           WHERE m.status = 1 ORDER BY m.sort_order, m.model_name"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let groups = sqlx::query!(
        r#"SELECT group_code AS code, description AS name, group_ratio::text AS ratio
           FROM price_groups ORDER BY sort_order, group_code"#
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(Json(json!({
        "models": models.into_iter().map(|m| json!({
            "model": m.model_name,
            "display_name": m.display_name,
            "vendor": m.vendor,
            "mode": m.pricing_mode,
            "model_ratio": m.model_ratio,
            "completion_ratio": m.completion_ratio,
            "cache_ratio": m.cache_ratio,
            "cache_write_ratio": m.cache_write_ratio,
            "audio_ratio": m.audio_ratio,
            "audio_completion_ratio": m.audio_completion_ratio,
            "image_ratio": m.image_ratio,
            "per_call_price_micro": m.per_call_price_micro,
        })).collect::<Vec<_>>(),
        "groups": groups.into_iter().map(|g| json!({
            "code": g.code, "name": g.name, "ratio": g.ratio,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// 游标：取该 id 之前的记录（created_at 倒序翻页）。
    #[serde(default)]
    pub before: Option<i64>,
}

fn default_limit() -> i64 {
    50
}

/// GET /api/me/logs：本用户账单明细（含 pricing_snapshot——前端账单解释器数据源）。
pub async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let limit = q.limit.clamp(1, 200);
    let rows = sqlx::query!(
        r#"SELECT id, request_id, model_name, log_type, status,
                  prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
                  amount_micro, original_amount_micro, discount_micro,
                  pricing_snapshot, error_code, latency_ms, is_stream, created_at
           FROM billing_records
           WHERE user_id = $1 AND ($2::bigint IS NULL OR id < $2)
           ORDER BY id DESC LIMIT $3"#,
        key.user_id,
        q.before,
        limit
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let next_before = rows.last().map(|r| r.id);
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "request_id": r.request_id,
                "model": r.model_name,
                "log_type": r.log_type,
                "status": r.status,
                "usage": {
                    "prompt_tokens": r.prompt_tokens,
                    "cached_tokens": r.cached_tokens,
                    "completion_tokens": r.completion_tokens,
                    "reasoning_tokens": r.reasoning_tokens,
                },
                "amount_micro": r.amount_micro,
                "original_amount_micro": r.original_amount_micro,
                "discount_micro": r.discount_micro,
                "pricing_snapshot": r.pricing_snapshot,
                "error_code": r.error_code,
                "latency_ms": r.latency_ms,
                "is_stream": r.is_stream,
                "created_at": r.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "next_before": next_before })))
}

#[derive(Deserialize)]
pub struct RedeemReq {
    pub code: String,
}

/// POST /api/me/redeem：兑换码核销（行级原子，一次性；credit 事件 actor=system:redeem）。
pub async fn redeem(
    State(state): State<AppState>,
    conn: crate::console::auth_web::MaybeConnectInfo,
    headers: HeaderMap,
    Json(req): Json<RedeemReq>,
) -> Result<Json<Value>, AppError> {
    // 兑换码爆破面：每 IP 限速（对齐 new-api rc.24 关键路由限流）
    crate::console::auth_web::critical_rate_guard(&state, &headers, conn.0.as_ref(), "redeem", 10)
        .await?;
    let key = authenticate(&state, &headers).await?;
    let code = req.code.trim();

    // per-IP 闸（#1790-5）：翻转前预查批次限额；IP 取 CDN 头（直连无头不限）
    let precheck = okapi_store::admin::redemption_precheck(&state.pg, code).await?;
    let mut ip_charge: Option<(uuid::Uuid, String)> = None;
    if let Some(pre) = &precheck
        && let Some(cap) = pre.max_per_ip
        && let Some(ip) = crate::gateway::clients::detect_client_ip(&headers)
    {
        let count = state.sched.redeem_ip_incr(pre.batch_id, &ip).await;
        if count > i64::from(cap) {
            state.sched.redeem_ip_decr(pre.batch_id, &ip).await;
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                okapi_api::codes::RATE_LIMITED,
            )
            .with_param("redeem_ip"));
        }
        ip_charge = Some((pre.batch_id, ip));
    }

    let claimed = okapi_store::admin::claim_redemption(&state.pg, code, key.user_id).await?;
    let Some(claimed) = claimed else {
        // 预查通过但翻转失败（竞争被抢/绑定他人）：回退 IP 计数
        if let Some((batch, ip)) = ip_charge {
            state.sched.redeem_ip_decr(batch, &ip).await;
        }
        return Err(AppError::new(StatusCode::NOT_FOUND, "redemption_invalid"));
    };

    let amount = okapi_domain::Money::from_micros(claimed.amount_micro);
    let balance_after = state.ledger.credit(key.user_id, amount).await?;
    okapi_ledger::pg::record_credit(
        &state.pg,
        key.user_id,
        amount,
        "adjust",
        "system:redeem",
        json!({
            "tags": ["redemption"],
            "code_id": claimed.code_id,
            "plan_code": claimed.plan_code,
        }),
    )
    .await?;

    // 套餐附带语义：加组失败/有效期失败不回滚入账（记日志走人工），核销主流程已成立
    if let Some(group) = &claimed.grant_group
        && let Err(err) = okapi_store::admin::add_user_group(&state.pg, key.user_id, group).await
    {
        tracing::error!(user_id = key.user_id, group, error = %err, "套餐加组失败（人工跟进）");
    }
    if let Some(days) = claimed.balance_valid_days {
        let expires = chrono::Utc::now() + chrono::Duration::days(i64::from(days));
        let result = sqlx::query!(
            r#"UPDATE users SET balance_expires_at = $2, updated_at = now() WHERE id = $1"#,
            key.user_id,
            expires
        )
        .execute(&state.pg)
        .await;
        if let Err(err) = result {
            tracing::error!(user_id = key.user_id, error = %err, "套餐余额有效期设置失败（人工跟进）");
        }
    }

    Ok(Json(json!({
        "amount_micro": claimed.amount_micro,
        "balance_after_micro": balance_after.as_micros(),
        "plan_code": claimed.plan_code,
        "granted_group": claimed.grant_group,
        "balance_valid_days": claimed.balance_valid_days,
    })))
}

/// GET /api/me/keys：本用户全部 key 及累计分账（合作商查员工用量）。
pub async fn keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let rows = sqlx::query!(
        r#"
        SELECT id, name, key_prefix, status, used_micro, rpm_limit, tpm_limit, rpd_limit,
               daily_token_limit, max_concurrency, model_allowlist, group_override,
               expires_at, last_used_at, created_at
        FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL ORDER BY id
        "#,
        key.user_id
    )
    .fetch_all(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    // CH 分账（可用时补充按 key 聚合；不可用时仅返回 PG used_micro 累计列）
    let mut ch_usage: Vec<Value> = Vec::new();
    if let Some(ch) = &state.ch {
        let ids: Vec<String> = rows.iter().map(|r| r.id.to_string()).collect();
        if !ids.is_empty() {
            let sql = format!(
                "SELECT api_key_id, sumMerge(amount) AS amount_micro, countMerge(requests) AS requests \
                 FROM mv_apikey_day WHERE api_key_id IN ({}) GROUP BY api_key_id",
                ids.join(",")
            );
            ch_usage = ch.query_json_each_row(&sql).await.unwrap_or_default();
        }
    }

    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let agg = ch_usage.iter().find(|u| ch_i64(u, "api_key_id") == r.id);
            json!({
                "id": r.id,
                "name": r.name,
                "key_prefix": r.key_prefix,
                "status": r.status,
                "used_micro": r.used_micro,
                "rpm_limit": r.rpm_limit,
                "tpm_limit": r.tpm_limit,
                "rpd_limit": r.rpd_limit,
                "daily_token_limit": r.daily_token_limit,
                "max_concurrency": r.max_concurrency,
                "model_allowlist": r.model_allowlist,
                "group_override": r.group_override,
                "expires_at": r.expires_at,
                "last_used_at": r.last_used_at,
                "created_at": r.created_at,
                "amount_micro": agg.map_or(0, |u| ch_i64(u, "amount_micro")),
                "requests": agg.map_or(0, |u| ch_i64(u, "requests")),
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

/// 自助面可改字段：全部为"收窄自己这把 key"的语义，不含限额与分组覆盖
/// （那两类是管控项与计价锚点，放开等于用户可自行提额/改价，仅管理面可写）。
#[derive(Deserialize)]
pub struct PatchKeyReq {
    #[serde(default)]
    pub name: Option<String>,
    /// 1=启用 2=停用。
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub expires_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    /// 字符串数组；null = 解除模型限制。
    #[serde(default, deserialize_with = "super::double_option")]
    pub model_allowlist: Option<Option<Vec<String>>>,
}

/// 模型白名单归一化：空数组等价于"不限"，避免落成一把谁也调不通的死 key。
pub(super) fn normalize_allowlist(list: Option<Vec<String>>) -> Option<Value> {
    let items: Vec<String> = list?
        .into_iter()
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty())
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(json!(items))
}

/// PATCH /api/me/keys/{id}：改自己 key 的名称/启停/过期/模型白名单。
pub async fn patch_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<PatchKeyReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    if let Some(status) = req.status
        && !matches!(status, 1 | 2)
    {
        return Err(AppError::bad_request().with_param("status"));
    }
    let patch = okapi_store::admin::ApiKeyPatch {
        name: req.name.map(|n| n.trim().to_owned()),
        status: req.status,
        expires_at: req.expires_at,
        model_allowlist: req.model_allowlist.map(normalize_allowlist),
        ..Default::default()
    };
    let touched =
        okapi_store::admin::patch_api_key(&state.pg, id, Some(key.user_id), &patch).await?;
    let Some(touched) = touched else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    // 先落库后失效：并发回源读到的必是新值
    state.sched.auth_del(&touched.key_hash).await;
    Ok(Json(json!({ "ok": true, "key_id": id })))
}

/// DELETE /api/me/keys/{id}：吊销自己的 key（软删除，明文 key 立即失效）。
pub async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let touched = okapi_store::admin::soft_delete_api_key(&state.pg, id, Some(key.user_id)).await?;
    let Some(touched) = touched else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    state.sched.auth_del(&touched.key_hash).await;
    Ok(Json(json!({ "ok": true, "key_id": id })))
}

/// GET /api/me/aff：邀请码（惰性生成）+ 邀请人数 + 累计返利（M4 aff）。
pub async fn aff(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let code = sqlx::query_scalar!(
        r#"SELECT aff_code FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        key.user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .flatten();
    let code = if let Some(code) = code {
        code
    } else {
        // 惰性生成：8 位小写字母数字；唯一索引冲突重试
        loop {
            let candidate: String = {
                use rand::RngExt;
                use rand::distr::Alphanumeric;
                rand::rng()
                    .sample_iter(&Alphanumeric)
                    .take(8)
                    .map(|c| (c as char).to_ascii_lowercase())
                    .collect()
            };
            let updated = sqlx::query!(
                r#"UPDATE users SET aff_code = $2 WHERE id = $1 AND aff_code IS NULL"#,
                key.user_id,
                candidate
            )
            .execute(&state.pg)
            .await;
            match updated {
                Ok(r) if r.rows_affected() == 1 => break candidate,
                Ok(_) => {
                    // 并发已生成：读回
                    if let Ok(Some(Some(existing))) = sqlx::query_scalar!(
                        r#"SELECT aff_code FROM users WHERE id = $1"#,
                        key.user_id
                    )
                    .fetch_optional(&state.pg)
                    .await
                    {
                        break existing;
                    }
                }
                Err(_) => {} // 唯一冲突：换码重试
            }
        }
    };

    let invitees = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM users WHERE inviter_id = $1 AND deleted_at IS NULL"#,
        key.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let reward_sum = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(delta_micro), 0)::bigint AS "s!"
           FROM billing_events WHERE user_id = $1 AND actor = 'system:aff'"#,
        key.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    Ok(Json(json!({
        "aff_code": code,
        "invitees": invitees,
        "reward_sum_micro": reward_sum,
    })))
}
