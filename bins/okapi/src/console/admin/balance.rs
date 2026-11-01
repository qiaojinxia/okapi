//! 余额操作：充值 / 扣减、个人系数、余额到期、按请求退款。
use super::{audit, guard};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use crate::kernel::extract::Path;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_domain::Money;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

// ---- 用户入账 ----

#[derive(Deserialize)]
pub struct CreditReq {
    pub amount_micro: i64,
    #[serde(default)]
    pub reason: String,
}

#[derive(Deserialize)]
pub struct SetMultiplierReq {
    /// 十进制字符串（如 "1.25"、"0.8"）。0 = 免单；禁浮点入库故不收 f64。
    pub multiplier: String,
}

/// POST /admin/users/{id}/multiplier：设用户个人计价系数。
///
/// 权限走 `pricing.write` 而非 `user.manage`：它是计价链上与模型倍率、分组倍率并列的
/// 一个乘数（`amount = eff × model × group × **user** × 规则`），改它等于改这个人的价目表。
///
/// 取值 [0, 1000]：0 = 免单（内部账号常用），上限只是防手滑把小数点点丢。
/// 改完必须刷鉴权缓存——系数随 `AuthedKey` 缓存，不刷的话最长一分钟仍按旧价计费。
pub async fn set_user_multiplier(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    ExtractJson(req): ExtractJson<SetMultiplierReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let raw = req.multiplier.trim();
    let value: okapi_pricing::RatioFp = raw
        .parse()
        .map_err(|_| AppError::bad_request().with_param("multiplier"))?;
    if value.as_scaled() > 1_000_000_000 {
        return Err(AppError::bad_request().with_param("multiplier"));
    }
    if !okapi_store::admin::set_user_multiplier(&state.pg, user_id, raw).await? {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "user.set_multiplier",
        &user_id.to_string(),
        json!({ "multiplier": raw }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "multiplier": raw })))
}

/// `numeric(12,6)` 倍率列能存的最大值（scale=1e6 的定点）。model_pricing / price_groups 的倍率列都是这个类型，
/// 超出的值解析得过、落库时才溢出成 500。
pub(super) const RATIO_COLUMN_MAX_SCALED: i64 = 999_999_999_999;

/// 倍率字面量：能解析、非负，且放得进倍率列。
pub(in crate::console) fn ratio_fits(literal: &str) -> bool {
    literal
        .parse::<okapi_pricing::RatioFp>()
        .is_ok_and(|v| v.as_scaled() <= RATIO_COLUMN_MAX_SCALED)
}

/// 兑换码面值的合法范围：1..=`MAXIMUM_MICROS`（账本与 Lua 的定量上限）。
/// 超限的码生成时不拦，核销时才在入账处永久报错，码也就作废了。
pub(in crate::console) fn ensure_redemption_amount(amount_micro: i64) -> Result<(), AppError> {
    if (1..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&amount_micro) {
        Ok(())
    } else {
        Err(AppError::bad_request().with_param("count_or_amount"))
    }
}

/// 人工调账金额的合法范围：与其它资金入口同一条不变量（|金额| ≤ `MAXIMUM_MICROS`，账本与 Lua 的定量上限）。
/// 越界值此前一路送进账本，在 bigint 加法或入账后的余额检查里变成 500，这里先挡成 400。
pub(in crate::console) fn ensure_adjust_amount(amount_micro: i64) -> Result<(), AppError> {
    let max = okapi_ledger::holds::MAXIMUM_MICROS;
    if (-max..=max).contains(&amount_micro) {
        Ok(())
    } else {
        Err(AppError::bad_request().with_param("amount_micro"))
    }
}

pub async fn credit_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    ExtractJson(req): ExtractJson<CreditReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    if req.amount_micro == 0 {
        return Err(AppError::bad_request().with_param("amount_micro"));
    }
    ensure_adjust_amount(req.amount_micro)?;
    // 管理台契约（BalanceSection 提示语）："正数为充值，负数为扣减"。负数走
    // debit：按可用余额钳制（与 expire 同语义），事件仍是 adjust 的**负额**——
    // 现金流的 clawed 统计正是按负额 adjust 聚合的。
    let admin = format!("admin:{}", actor.actor_user_id());
    let (applied_micro, receipt, action) = if req.amount_micro > 0 {
        let receipt = okapi_ledger::operations::credit(
            &state.pg,
            &state.ledger,
            user_id,
            Money::from_micros(req.amount_micro),
            "adjust",
            &admin,
            json!({ "tags": ["manual_credit"], "reason": req.reason }),
        )
        .await?;
        (req.amount_micro, Some(receipt), "user.credit")
    } else {
        let (applied, receipt) = okapi_ledger::operations::debit(
            &state.pg,
            &state.ledger,
            user_id,
            Money::from_micros(req.amount_micro.saturating_neg()),
            "adjust",
            &admin,
            json!({ "tags": ["manual_debit"], "reason": req.reason }),
        )
        .await?;
        (applied.as_micros().saturating_neg(), receipt, "user.debit")
    };
    audit(
        &state,
        &actor,
        action,
        &user_id.to_string(),
        json!({ "amount_micro": req.amount_micro, "applied_micro": applied_micro, "reason": req.reason }),
    )
    .await;
    Ok(Json(json!({
        "balance_after_micro": receipt.as_ref().and_then(|r| r.balance_after).map(okapi_domain::Money::as_micros),
        "operation_id": receipt.as_ref().map(|r| r.operation_id),
        "pending": receipt.as_ref().is_some_and(|r| r.balance_after.is_none()),
        "applied_micro": applied_micro,
    })))
}

#[derive(Deserialize)]
pub struct BalanceExpiryReq {
    /// RFC3339；null = 取消有效期（永不过期）。
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 设置/取消余额有效期（#1790-6）：到期由 worker 清零并事件留痕。
pub async fn set_balance_expiry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    ExtractJson(req): ExtractJson<BalanceExpiryReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    let updated = sqlx::query!(
        r#"UPDATE users SET balance_expires_at = $2, updated_at = now()
           WHERE id = $1 AND deleted_at IS NULL"#,
        user_id,
        req.expires_at
    )
    .execute(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    if updated.rows_affected() == 0 {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    audit(
        &state,
        &actor,
        "user.balance_expiry",
        &user_id.to_string(),
        json!({ "expires_at": req.expires_at }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 按日志退款 ----

#[derive(Deserialize)]
pub struct RefundReq {
    pub request_id: Uuid,
    #[serde(default)]
    pub reason: String,
}

/// 按 request_id 查账单摘要（billing.read）：退款前的预览步——
/// 让管理员先看清"退的是谁的哪笔、多少钱、当前什么状态"，而不是对着一个
/// UUID 盲按退款按钮。LEFT JOIN 用户名，退款资格由 status 判定。
pub async fn billing_record_lookup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::BILLING_READ).await?;
    let row = sqlx::query!(
        r#"
        SELECT br.user_id, u.username, br.model_name, br.status, br.log_type,
               br.amount_micro, br.original_amount_micro,
               br.prompt_tokens, br.completion_tokens,
               br.error_code, br.created_at
        FROM billing_records br
        LEFT JOIN users u ON u.id = br.user_id
        WHERE br.request_id = $1
        ORDER BY br.created_at DESC
        LIMIT 1
        "#,
        request_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let Some(r) = row else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("request_id"));
    };
    Ok(Json(json!({
        "request_id": request_id,
        "user_id": r.user_id,
        "username": r.username,
        "model": r.model_name,
        "status": r.status,
        "log_type": r.log_type,
        "amount_micro": r.amount_micro,
        "original_amount_micro": r.original_amount_micro,
        "prompt_tokens": r.prompt_tokens,
        "completion_tokens": r.completion_tokens,
        "error_code": r.error_code,
        "created_at": r.created_at,
        // 只有成功扣费（committed）的记录有款可退
        "refundable": r.status == 20,
    })))
}

/// 按日志退款（§5.3）：事件溯源冲销，账单/统计/余额三处口径自动一致；幂等。
/// 三种结局分开说（此前"已退过"与"id 不存在"都混在 404 里，管理员分不清
/// "打错了"还是"重复点了但幂等安全"）：
/// - 成功 → outcome=refunded + 金额与退款后余额；
/// - 已退过 → 200 outcome=already_refunded（幂等语义，不是错误）；
/// - 存在但未成功扣费（失败/预扣中）→ 409 refund_not_committed（无款可退）；
/// - 不存在 → 404。
pub async fn refund_by_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<RefundReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::BILLING_REFUND).await?;
    let outcome = okapi_ledger::operations::refund(
        &state.pg,
        &state.ledger,
        req.request_id,
        &req.reason,
        &format!("admin:{}", actor.actor_user_id()),
    )
    .await
    .map_err(AppError::from)?;
    let Some((refund, balance_after)) = outcome else {
        let mut history = okapi_store::history::read(&state.pg).await?;
        let status = sqlx::query_scalar!(
            r#"SELECT status AS "status!" FROM billing_financial_records WHERE request_id = $1
               ORDER BY created_at DESC LIMIT 1"#,
            req.request_id
        )
        .fetch_optional(&mut *history)
        .await
        .map_err(okapi_store::StoreError::from)?;
        history
            .commit()
            .await
            .map_err(okapi_store::StoreError::from)?;
        return match status {
            Some(30) => Ok(Json(json!({ "outcome": "already_refunded" }))),
            Some(_) => Err(AppError::new(StatusCode::CONFLICT, "refund_not_committed")),
            None => {
                Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("request_id"))
            }
        };
    };
    audit(
        &state,
        &actor,
        "billing.refund",
        &req.request_id.to_string(),
        json!({
            "amount_micro": refund.amount.as_micros(),
            "reason": req.reason,
            "pool": refund.pool.as_i16(),
        }),
    )
    .await;
    Ok(Json(json!({
        "outcome": "refunded",
        "user_id": refund.user_id,
        "refunded_micro": refund.amount.as_micros(),
        "credited_micro": refund.credit.as_micros(),
        "balance_after_micro": balance_after.balance_after.map(okapi_domain::Money::as_micros), "operation_id": balance_after.operation_id, "pending": balance_after.balance_after.is_none(),
    })))
}
