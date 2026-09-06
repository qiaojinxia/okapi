//! 订阅套餐端点（IMPLEMENTATION §11.28）：门户套餐页 / 我的订阅 / 购买下单；管理端发放 / 取消。
//!
//! 编排逻辑（PG 翻转 → 第二池 → 事件）在 `okapi_ledger::subscriptions`，这里只做 HTTP 与
//! 鉴权缓存失效（分组变了就 `auth_flush`，网关 60s 缓存立刻作废）。

use super::admin::{audit, guard};
use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_ledger::subscriptions::{self as flow, Granted};
use okapi_store::subscriptions::{self as store, SubPlan, Subscription};
use serde::Deserialize;
use serde_json::{Value, json};

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
) -> Result<Granted, AppError> {
    let granted = flow::grant(&state.pg, &state.ledger, user_id, plan, source, actor).await?;
    if granted.group_changed() {
        state.sched.auth_flush().await;
    }
    tracing::info!(
        user_id,
        plan = %plan.plan_code,
        outcome = granted.kind(),
        expires_at = %granted.subscription().expires_at,
        "订阅授予"
    );
    Ok(granted)
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
    let (remaining, until) = state.ledger.sub_balance(sub.user_id).await?;
    Ok(json!({
        "id": sub.id,
        "plan_code": sub.plan_code,
        "display_name": sub.display_name,
        "period": sub.period,
        "status": sub.status,
        "quota_micro": sub.quota_micro,
        // 池允许单笔越界为负；对用户展示钳到 0
        "remaining_micro": remaining.as_micros().max(0),
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

fn plan_view(p: &SubPlan) -> Value {
    json!({
        "plan_code": p.plan_code,
        "display_name": p.display_name,
        "quota_micro": p.quota_micro,
        "price_micro": p.price_micro,
        "purchasable": p.price_micro > 0,
        "period": p.period,
        "duration_days": p.duration_days,
        "group_code": p.group_code,
        "description": p.description,
        "sort_order": p.sort_order,
    })
}

async fn current_and_history(state: &AppState, user_id: i64) -> Result<Value, AppError> {
    let current = match store::active_for_user(&state.pg, user_id).await? {
        Some(sub) => Some(view(state, &sub).await?),
        None => None,
    };
    let history = store::history_for_user(&state.pg, user_id, 20).await?;
    Ok(json!({ "subscription": current, "history": history }))
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
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    Ok(Json(current_and_history(&state, key.user_id).await?))
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
    Json(req): Json<CheckoutReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let plan = store::find_sub_plan(&state.pg, req.plan_code.trim())
        .await?
        .ok_or_else(|| {
            AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("plan_code")
        })?;
    if plan.price_micro <= 0 {
        return Err(AppError::new(StatusCode::BAD_REQUEST, PLAN_NOT_PURCHASABLE));
    }
    if let Some(active) = store::active_for_user(&state.pg, key.user_id).await?
        && active.plan_id != plan.id
    {
        return Err(
            AppError::new(StatusCode::CONFLICT, SUBSCRIPTION_ACTIVE).with_param(active.plan_code)
        );
    }
    let order = super::pay::place_order(
        &state,
        key.user_id,
        plan.price_micro,
        &req.gateway,
        Some(plan.id),
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
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::USER_READ).await?;
    Ok(Json(current_and_history(&state, user_id).await?))
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
    Json(req): Json<AdminGrantReq>,
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
    let plan = store::find_sub_plan(&state.pg, req.plan_code.trim())
        .await?
        .ok_or_else(|| {
            AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("plan_code")
        })?;
    let actor_tag = format!("admin:{}", actor.user_id);
    let granted = grant(&state, user_id, &plan, &actor_tag, &actor_tag).await?;
    audit(
        &state,
        &actor,
        "subscription.grant",
        &user_id.to_string(),
        json!({ "plan_code": plan.plan_code, "outcome": granted.kind() }),
    )
    .await;
    let body = view(&state, granted.subscription()).await?;
    Ok(Json(
        json!({ "outcome": granted.kind(), "subscription": body }),
    ))
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
