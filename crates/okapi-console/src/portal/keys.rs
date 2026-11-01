//! 门户 API key：列表与额度、按 key 的用量、改（限额 / IP 白名单）与删。
use super::super::query::{PageQuery, Query};
use super::{ch_i64, ensure_selectable, key_trends};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_gateway::state::AppState;
use okapi_kernel::auth::authenticate;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_kernel::extract::Path;
use serde::Deserialize;
use serde_json::{Value, json};

/// GET /api/me/keys：本用户的 key 及累计分账（合作商查员工用量）；`limit/offset` 可选切片，
/// 默认每页 20 条并返回总数。
pub async fn keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let slice = q.slice();
    let delegated_key = key.member_user_id.map(|_| key.key_id);
    // 登录 key（session_hash 非空）不在这里：它们随登录会话在「登录设备」里管理
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT id, name, key_prefix, status, used_micro, rpm_limit, tpm_limit, rpd_limit,
               daily_token_limit, max_concurrency, model_allowlist, group_override, ip_allowlist,
               expires_at, last_used_at, created_at
        FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL AND session_hash IS NULL
          AND ($4::bigint IS NULL OR id=$4) ORDER BY id
        LIMIT $2 OFFSET $3
        "#,
            key.user_id,
            slice.limit,
            slice.offset,
            delegated_key
        )
        .fetch_all(&state.pg),
        okapi_store::listing::count_unless_all(
            slice,
            sqlx::query_scalar!(
                r#"SELECT COUNT(*)::bigint AS "c!" FROM api_keys
           WHERE user_id = $1 AND deleted_at IS NULL AND session_hash IS NULL
             AND ($2::bigint IS NULL OR id=$2)"#,
                key.user_id,
                delegated_key
            )
            .fetch_one(&state.pg)
        ),
    )
    .map_err(okapi_store::StoreError::from)?;
    let total = total.unwrap_or_else(|| okapi_store::listing::len_as_total(rows.len()));

    let key_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let quotas = key_quotas(&state.pg, key.user_id, &key_ids).await?;
    let saved_ids = okapi_store::api_key_secret::saved_ids(&state.pg, key.user_id, &key_ids)
        .await
        .map_err(|err| tracing::warn!(error = %err, "key copy metadata unavailable"))
        .ok();
    let trends = key_trends::load(
        &state.pg,
        key.user_id,
        &key_ids,
        chrono::Utc::now().date_naive(),
    )
    .await
    .map_err(|err| tracing::warn!(error = %err, "key Token trends unavailable"))
    .ok();

    let ch_usage = key_aggregate_usage(state.ch.as_ref(), &key_ids).await;

    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let agg = ch_usage.iter().find(|u| ch_i64(u, "api_key_id") == r.id);
            json!({
                "id": r.id,
                "name": r.name,
                "key_prefix": r.key_prefix,
                "copy_status": match &saved_ids {
                    Some(ids) if !ids.contains(&r.id) => "not_saved",
                    Some(_) if state.master_key.is_some() => "available",
                    _ => "unavailable",
                },
                "status": r.status,
                "used_micro": r.used_micro,
                "quota_mode": quotas.get(&r.id).map_or(0, |q| q.0),
                "quota_micro": quotas.get(&r.id).and_then(|q| q.1),
                "rpm_limit": r.rpm_limit,
                "tpm_limit": r.tpm_limit,
                "rpd_limit": r.rpd_limit,
                "daily_token_limit": r.daily_token_limit,
                "max_concurrency": r.max_concurrency,
                "model_allowlist": r.model_allowlist,
                "group_override": r.group_override,
                "ip_allowlist": r.ip_allowlist,
                "expires_at": r.expires_at,
                "last_used_at": r.last_used_at,
                "created_at": r.created_at,
                "amount_micro": agg.map_or(0, |u| ch_i64(u, "amount_micro")),
                "requests": agg.map_or(0, |u| ch_i64(u, "requests")),
                "usage_trend": trends.as_ref().and_then(|trends| trends.get(&r.id)),
            })
        })
        .collect();
    Ok(Json(
        json!({ "data": data, "total": total, "key_limits_supported": true }),
    ))
}

pub(super) async fn key_quotas(
    pg: &sqlx::PgPool,
    user_id: i64,
    ids: &[i64],
) -> Result<std::collections::BTreeMap<i64, (i16, Option<i64>)>, AppError> {
    let rows: Vec<(i64, i16, Option<i64>)> = sqlx::query_as(
        "SELECT id, quota_mode, quota_micro FROM api_keys WHERE user_id=$1 AND id=ANY($2)",
    )
    .bind(user_id)
    .bind(ids)
    .fetch_all(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(rows
        .into_iter()
        .map(|(id, mode, quota)| (id, (mode, quota)))
        .collect())
}

/// CH unavailable preserves the existing PG-only key listing behavior.
pub(super) async fn key_aggregate_usage(
    ch: Option<&okapi_store::ChClient>,
    ids: &[i64],
) -> Vec<Value> {
    let Some(ch) = ch.filter(|_| !ids.is_empty()) else {
        return Vec::new();
    };
    let ids = ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT api_key_id, sumMerge(amount) AS amount_micro, countMerge(requests) AS requests FROM mv_apikey_day WHERE api_key_id IN ({ids}) GROUP BY api_key_id"
    );
    ch.query_json_each_row(&sql).await.unwrap_or_default()
}

/// 自助面可改字段：收窄自己这把 key（名字 / 状态 / 过期 / 白名单），外加**在可选集合内**
/// 换分组——分组是计价锚点，但可选集合由管理员通过 `self_select` 与用户分组划定，
/// 用户只能在划定的分组里挑，不构成自行改价；密钥额度不改变账户余额。
#[derive(Deserialize)]
pub struct PatchKeyReq {
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub quota_micro: Option<Option<i64>>,
    #[serde(default)]
    pub name: Option<String>,
    /// 1=启用 2=停用。
    #[serde(default)]
    pub status: Option<i16>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub expires_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    /// 字符串数组；null = 解除模型限制。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub model_allowlist: Option<Option<Vec<String>>>,
    /// 分组：字符串 = 选定分组（须在 /api/me/groups 可选集合内）；null = 跟随用户分组。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub group_code: Option<Option<String>>,
    /// IP 白名单：地址或 CIDR 数组；null / 空数组 = 解除限制。每条须可解析，否则 400——
    /// 一条拼错的白名单会把 key 锁死而毫无提示。
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub ip_allowlist: Option<Option<Vec<String>>>,
}

/// IP 白名单归一化：去空白、去重、逐条校验；空数组等价于"不限"（存 null）。
pub(crate) fn normalize_ip_allowlist(list: Option<Vec<String>>) -> Result<Option<Value>, AppError> {
    let Some(list) = list else {
        return Ok(None);
    };
    let mut out: Vec<String> = Vec::new();
    for raw in list {
        let entry = raw.trim().to_owned();
        if entry.is_empty() || out.contains(&entry) {
            continue;
        }
        if !okapi_store::netmatch::is_valid_entry(&entry) {
            return Err(AppError::bad_request().with_param(format!("ip_allowlist:{entry}")));
        }
        out.push(entry);
    }
    Ok((!out.is_empty()).then(|| json!(out)))
}

/// 模型白名单归一化：空数组等价于"不限"，避免落成一把谁也调不通的死 key。
pub(crate) fn normalize_allowlist(list: Option<Vec<String>>) -> Option<Value> {
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

pub(crate) fn validate_key_limits(
    quota: Option<i64>,
    expires: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), AppError> {
    if quota.is_some_and(|v| !(1..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&v)) {
        return Err(AppError::bad_request().with_param("quota_micro"));
    }
    if expires.is_some_and(|v| v <= chrono::Utc::now()) {
        return Err(AppError::bad_request().with_param("expires_at"));
    }
    Ok(())
}

/// PATCH /api/me/keys/{id}：改自己 key 的名称/启停/过期/模型白名单。
pub async fn patch_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(req): ExtractJson<PatchKeyReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    // A delegated team credential manages itself. Owner/admin management of
    // other members uses the session-authenticated /api/teams endpoints.
    if key.member_user_id.is_some() && id != key.key_id {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::PERMISSION_DENIED,
        ));
    }
    validate_key_limits(req.quota_micro.flatten(), req.expires_at.flatten())?;
    // A delegated/restricted credential must not remove its own restrictions.
    let changes_access = req.quota_micro.is_some()
        || req.expires_at.is_some()
        || req.model_allowlist.is_some()
        || req.group_code.is_some()
        || req.ip_allowlist.is_some();
    if changes_access
        && (key.quota_limited
            || key.model_allowlist.is_some()
            || key.expires_at.is_some()
            || key.ip_allowlist.is_some())
        && super::super::auth_web::require_session(&state, &headers)
            .await
            .ok()
            != Some(key.user_id)
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "key_limits_session_required",
        ));
    }
    if let Some(status) = req.status
        && !matches!(status, 1 | 2)
    {
        return Err(AppError::bad_request().with_param("status"));
    }
    // 停用当前登录用的这把 key = 把自己锁在门外（同删除）
    if req.status == Some(2) && id == key.key_id {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            codes::CURRENT_KEY_IN_USE,
        ));
    }
    let group_override = match req.group_code {
        Some(Some(code)) => {
            let code = code.trim().to_owned();
            ensure_selectable(&state, key.user_id, &code).await?;
            Some(Some(code))
        }
        Some(None) => Some(None),
        None => None,
    };
    let ip_allowlist = match req.ip_allowlist {
        Some(list) => Some(normalize_ip_allowlist(list)?),
        None => None,
    };
    let patch = okapi_store::admin::ApiKeyPatch {
        quota_micro: req.quota_micro,
        name: req
            .name
            .as_deref()
            .map(|name| {
                super::super::identifiers::ensure_optional_text("name", name, 128)
                    .map(str::to_owned)
            })
            .transpose()?,
        status: req.status,
        expires_at: req.expires_at,
        model_allowlist: req.model_allowlist.map(normalize_allowlist),
        group_override,
        ip_allowlist,
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
/// 当前请求自己用的那把不能删：删了当前登录立刻失效，界面上所有请求都会变成 invalid_api_key。
pub async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    if id == key.key_id {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            codes::CURRENT_KEY_IN_USE,
        ));
    }
    // A delegated team credential manages itself. Owner/admin management of
    // other members uses the session-authenticated /api/teams endpoints.
    if key.member_user_id.is_some() && id != key.key_id {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::PERMISSION_DENIED,
        ));
    }
    let touched = okapi_store::admin::soft_delete_api_key(&state.pg, id, Some(key.user_id)).await?;
    let Some(touched) = touched else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    state.sched.auth_del(&touched.key_hash).await;
    Ok(Json(json!({ "ok": true, "key_id": id })))
}
