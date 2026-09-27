//! 订阅套餐端点（IMPLEMENTATION §11.28）：门户套餐页 / 我的订阅 / 购买下单；管理端发放 / 取消。
//!
//! 编排逻辑（PG 权益与事件原子提交 → 第二池可恢复同步）在 `okapi_ledger::subscriptions`，这里只做 HTTP 与
//! 鉴权缓存失效（分组变了就 `auth_flush`，网关 60s 缓存立刻作废）。

use super::admin::{audit, guard};
use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::extract::{Json as ExtractJson, Query};
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_ledger::subscriptions::{self as flow, Receipt};
use okapi_store::subscriptions::{self as store, SubPlan, Subscription};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 售价为 0 的套餐不可自助购买（只能兑换码 / 管理员发放）。
pub const PLAN_NOT_PURCHASABLE: &str = "plan_not_purchasable";
/// 激活期内换套餐（升降级 backlog）；`LedgerError::SubscriptionActive` 映射到它。
pub const SUBSCRIPTION_ACTIVE: &str = "subscription_active";

/// 激活 / 续期（支付回调、兑换码核销、管理员发放共用）+ 鉴权缓存失效。
pub async fn grant(
    state: &AppState,
    user_id: i64,
    plan: &SubPlan,
    source: &str,
    actor: &str,
) -> Result<Receipt, AppError> {
    let receipt = flow::request(&state.pg, &state.ledger, user_id, plan, source, actor).await?;
    state.sched.auth_flush().await;
    Ok(receipt)
}

pub async fn receipt_view(state: &AppState, receipt: &Receipt) -> Result<Value, AppError> {
    let subscription = match &receipt.granted {
        Some(granted) if !receipt.pending => view(state, granted.subscription()).await?,
        Some(granted) => {
            let mut value =
                serde_json::to_value(granted.subscription()).map_err(|_| AppError::internal())?;
            value["remaining_micro"] = Value::Null;
            value["pool_until_unix"] = Value::Null;
            value
        }
        None => Value::Null,
    };
    Ok(json!({"operation_id":receipt.id,"pending":receipt.pending,
        "outcome":receipt.granted.as_ref().map_or("pending",flow::Granted::kind),"subscription":subscription}))
}

/// 结束订阅（3 取消）+ 鉴权缓存失效。
async fn cancel(
    state: &AppState,
    subscription_id: i64,
    actor: &str,
) -> Result<Option<Subscription>, AppError> {
    let ended = flow::end(&state.pg, &state.ledger, subscription_id, 3, actor).await?;
    if ended.as_ref().is_some_and(|s| s.granted_group) {
        state.sched.auth_flush().await;
    }
    Ok(ended)
}

/// 门户 / 管理端共用的订阅视图：PG 行 + Redis 池实时值。
pub async fn view(state: &AppState, sub: &Subscription) -> Result<Value, AppError> {
    let pending = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM subscription_sync WHERE user_id=$1) AS "pending!""#,
        sub.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let pending = pending && sub.status == 1;
    let (remaining, until) = if sub.status != 1 {
        (Some(0), Some(0))
    } else if pending {
        (None, None)
    } else {
        let (remaining, until) = state.ledger.sub_balance(sub.user_id).await?;
        (Some(remaining.as_micros().max(0)), Some(until))
    };
    Ok(json!({
        "id": sub.id,
        "plan_code": sub.plan_code,
        "display_name": sub.display_name,
        "period": sub.period,
        "status": sub.status,
        "quota_micro": sub.quota_micro,
        // 池允许单笔越界为负；对用户展示钳到 0
        "remaining_micro": remaining,
        "pending":pending,
        "group_code": sub.group_code,
        "granted_group": sub.granted_group,
        "starts_at": sub.starts_at,
        "expires_at": sub.expires_at,
        "window_start": sub.window_start,
        "window_end": sub.window_end,
        // Redis 侧可用截止 unix 秒（0 = 池当前不可用：worker 滚窗前的最多 60s 间隙）
        "pool_until_unix": until,
        "source": sub.source,
    }))
}

fn purchasable(p: &SubPlan) -> bool {
    (1..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&p.price_micro)
        && (1..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&p.quota_micro)
        && (1..=3).contains(&p.period)
        && store::expiry_after(chrono::Utc::now(), p.duration_days).is_ok()
}

fn plan_view(p: &SubPlan) -> Value {
    json!({
        "plan_code": p.plan_code,
        "display_name": p.display_name,
        "quota_micro": p.quota_micro,
        "price_micro": p.price_micro,
        "purchasable": purchasable(p),
        "period": p.period,
        "duration_days": p.duration_days,
        "group_code": p.group_code,
        "description": p.description,
        "sort_order": p.sort_order,
    })
}

#[derive(Deserialize, Default)]
pub struct SubscriptionQuery {
    pub pending_before: Option<i64>,
}
async fn current_and_history(
    state: &AppState,
    user_id: i64,
    before: Option<i64>,
) -> Result<Value, AppError> {
    if before.is_some_and(|n| n <= 0) {
        return Err(
            AppError::new(StatusCode::BAD_REQUEST, codes::BAD_REQUEST).with_param("pending_before")
        );
    }
    let current = match store::active_for_user(&state.pg, user_id).await? {
        Some(sub) => Some(view(state, &sub).await?),
        None => None,
    };
    let history = store::history_for_user(&state.pg, user_id, 20).await?;
    let mut pending=sqlx::query!("SELECT id,sequence,plan_snapshot,created_at,last_error FROM subscription_grants WHERE user_id=$1 AND applied_at IS NULL AND ($2::bigint IS NULL OR sequence<$2) ORDER BY sequence DESC LIMIT 21",user_id,before)
        .fetch_all(&state.pg).await.map_err(okapi_store::StoreError::from)?;
    let more = pending.len() > 20;
    pending.truncate(20);
    let next = if more {
        pending.last().map(|row| row.sequence)
    } else {
        None
    };
    let pending:Vec<_>=pending.into_iter().map(|row|json!({"operation_id":row.id,"plan_code":row.plan_snapshot["plan_code"],
        "display_name":row.plan_snapshot["display_name"],"accepted_at":row.created_at,"pending":true,"reason":row.last_error})).collect();
    Ok(
        json!({ "subscription": current, "history": history,"pending_grants":pending,"pending_next_before":next }),
    )
}

// ---- 门户 ----

/// GET /api/plans：启用中的订阅套餐（门户套餐页；需登录以免暴露商业配置）。
pub async fn list_public(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    authenticate(&state, &headers).await?;
    let plans = store::list_sub_plans(&state.pg).await?;
    Ok(Json(
        json!({ "data": plans.iter().map(plan_view).collect::<Vec<_>>() }),
    ))
}

/// GET /api/me/subscription：当前订阅（无则 `subscription: null`）+ 最近历史。
pub async fn mine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SubscriptionQuery>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    Ok(Json(
        current_and_history(&state, key.user_id, query.pending_before).await?,
    ))
}

#[derive(Deserialize)]
pub struct CheckoutReq {
    pub plan_code: String,
    /// epay | stripe
    pub gateway: String,
}

/// POST /api/me/subscriptions/checkout：建订阅购买单（与 `/api/me/topup` 同响应形状）。
///
/// 激活期内买别的套餐直接 409（不让用户付了钱再被拒）；同套餐 = 续期，允许。
pub async fn checkout(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<CheckoutReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let plan = store::find_sub_plan(&state.pg, req.plan_code.trim())
        .await?
        .ok_or_else(|| {
            AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("plan_code")
        })?;
    if !purchasable(&plan) {
        return Err(AppError::new(StatusCode::BAD_REQUEST, PLAN_NOT_PURCHASABLE));
    }
    let active = store::active_for_user(&state.pg, key.user_id).await?;
    if let Some(active) = &active
        && (active.plan_id != plan.id
            || active.quota_micro != plan.quota_micro
            || active.period != plan.period
            || active.group_code != plan.group_code)
    {
        return Err(
            AppError::new(StatusCode::CONFLICT, SUBSCRIPTION_ACTIVE).with_param(&active.plan_code)
        );
    }
    if let Some(active) = &active {
        store::expiry_after(
            active.expires_at.max(chrono::Utc::now()),
            plan.duration_days,
        )
        .map_err(|_| AppError::new(StatusCode::BAD_REQUEST, PLAN_NOT_PURCHASABLE))?;
    }
    let order = super::pay::place_order(
        &state,
        key.user_id,
        plan.price_micro,
        &req.gateway,
        Some(&plan),
        &format!("okapi_plan_{}", plan.plan_code),
    )
    .await?;
    Ok(Json(order))
}

// ---- 管理端 ----

/// GET /admin/users/{id}/subscription
pub async fn admin_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Query(query): Query<SubscriptionQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::USER_READ).await?;
    Ok(Json(
        current_and_history(&state, user_id, query.pending_before).await?,
    ))
}

#[derive(Deserialize)]
pub struct AdminGrantReq {
    pub plan_code: String,
}

/// POST /admin/users/{id}/subscription：发放 / 续期（免费；走 `USER_BALANCE_ADJUST`——它就是在给钱）。
pub async fn admin_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    ExtractJson(req): ExtractJson<AdminGrantReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL) AS "e!""#,
        user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if !exists {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    let actor_tag = format!("admin:{}", actor.user_id);
    let source = if let Some(key) = headers.get("Idempotency-Key") {
        let key = key.to_str().map_err(|_| {
            AppError::new(StatusCode::BAD_REQUEST, codes::BAD_REQUEST).with_param("idempotency_key")
        })?;
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(AppError::new(StatusCode::BAD_REQUEST, codes::BAD_REQUEST)
                .with_param("idempotency_key"));
        }
        format!(
            "{actor_tag}:{}",
            hex::encode(Sha256::digest(key.as_bytes()))
        )
    } else {
        format!("{actor_tag}:{}", uuid::Uuid::new_v4())
    };
    let accepted = sqlx::query_scalar!(
        "SELECT plan_snapshot FROM subscription_grants WHERE user_id=$1 AND source=$2",
        user_id,
        source
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let plan = if let Some(snapshot) = accepted {
        let plan: SubPlan = serde_json::from_value(snapshot).map_err(|_| AppError::internal())?;
        if plan.plan_code != req.plan_code.trim() {
            return Err(AppError::new(StatusCode::CONFLICT, codes::BAD_REQUEST)
                .with_param("idempotency_key"));
        }
        plan
    } else {
        store::find_sub_plan(&state.pg, req.plan_code.trim())
            .await?
            .ok_or_else(|| {
                AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("plan_code")
            })?
    };
    let receipt = grant(&state, user_id, &plan, &source, &actor_tag).await?;
    audit(
        &state,
        &actor,
        "subscription.grant",
        &user_id.to_string(),
        json!({ "plan_code": plan.plan_code, "operation_id": receipt.id, "pending":receipt.pending }),
    )
    .await;
    Ok(Json(receipt_view(&state, &receipt).await?))
}

/// DELETE /admin/users/{id}/subscription：立即结束（status 3）。无激活订阅 → 404。
pub async fn admin_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    let Some(active) = store::active_for_user(&state.pg, user_id).await? else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    let actor_tag = format!("admin:{}", actor.user_id);
    let ended = cancel(&state, active.id, &actor_tag).await?;
    audit(
        &state,
        &actor,
        "subscription.cancel",
        &user_id.to_string(),
        json!({ "plan_code": active.plan_code, "subscription_id": active.id }),
    )
    .await;
    Ok(Json(
        json!({ "ok": ended.is_some(), "subscription": ended }),
    ))
}
