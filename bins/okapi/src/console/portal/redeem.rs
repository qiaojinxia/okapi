//! 兑换码核销（钱包与订阅两类）。
use crate::gateway::state::AppState;
use crate::kernel::auth::authenticate;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;

#[derive(Deserialize)]
pub struct RedeemReq {
    pub code: String,
}

/// POST /api/me/redeem：兑换码核销（行级原子，一次性；credit 事件 actor=system:redeem）。
pub async fn redeem(
    State(state): State<AppState>,
    conn: crate::console::auth_web::MaybeConnectInfo,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<RedeemReq>,
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
        && let Some(ip) = crate::kernel::clients::detect_client_ip(&headers)
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

    // 走到这里本 IP 已经计了一次。没核销成功（拿锁、开事务、翻转、入账、提交任何一步失败）
    // 都退还，否则一次库故障就占掉这个 IP 7 天的配额；提交之后再失败不退，码已经用掉了。
    let (mut guard, claimed, accepted) = match accept_redemption(&state, key.user_id, code).await {
        Ok(accepted) => accepted,
        Err(error) => {
            if let Some((batch, ip)) = &ip_charge {
                state.sched.redeem_ip_decr(*batch, ip).await;
            }
            return Err(error);
        }
    };
    match accepted {
        Accepted::Subscription(id) => {
            let receipt =
                okapi_ledger::subscriptions::finish(&mut guard, &state.ledger, key.user_id, id)
                    .await;
            drop(guard);
            state.sched.auth_flush().await;
            // 码已经核销、订阅已经入队：回执视图读失败（PG / Redis 抖一下）不能回 500，
            // 否则用户以为没兑成、拿同一个码再试只会得到「兑换码无效」
            let mut body = match super::super::subscriptions::receipt_view(&state, &receipt).await {
                Ok(body) => body,
                Err(error) => {
                    tracing::error!(
                        status = %error.status, code = %error.code,
                        "订阅兑换码已生效，但回执渲染失败"
                    );
                    json!({"operation_id": receipt.id, "pending": true,
                        "outcome": "pending", "subscription": null})
                }
            };
            body["amount_micro"] = json!(0);
            body["plan_code"] = json!(claimed.plan_code);
            Ok(Json(body))
        }
        Accepted::Wallet(operation_id) => {
            let receipt = okapi_ledger::transfers::finish(
                &mut guard,
                &state.ledger,
                key.user_id,
                operation_id,
                okapi_ledger::Pool::Wallet,
            )
            .await;
            Ok(Json(json!({
                "amount_micro": claimed.amount_micro,
                "balance_after_micro": receipt.balance_after.map(okapi_domain::Money::as_micros),
                "operation_id": receipt.operation_id,
                "pending": receipt.balance_after.is_none(),
                "plan_code": claimed.plan_code,
                "granted_group": claimed.grant_group,
                "balance_valid_days": claimed.balance_valid_days,
            })))
        }
    }
}

/// 兑换码在事务里落定的去处：订阅入队（激活单号）或钱包入账（操作号）。
pub(super) enum Accepted {
    Subscription(uuid::Uuid),
    Wallet(uuid::Uuid),
}

/// 锁用户、翻转兑换码并在同一事务里入账，一直到提交。返回 Err 时兑换码没有被用掉。
pub(super) async fn accept_redemption(
    state: &AppState,
    user_id: i64,
    code: &str,
) -> Result<
    (
        okapi_ledger::holds::UserGuard,
        okapi_store::admin::ClaimedRedemption,
        Accepted,
    ),
    AppError,
> {
    let mut guard = okapi_ledger::holds::UserGuard::acquire(&state.pg, user_id).await?;
    let mut tx = guard
        .connection()?
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let Some(mut claimed) =
        okapi_store::admin::claim_redemption_in_tx(&mut tx, code, user_id).await?
    else {
        // 预查通过但翻转失败（竞争被抢 / 绑定他人）
        return Err(AppError::new(StatusCode::NOT_FOUND, "redemption_invalid"));
    };

    // 绑订阅套餐（§11.28）：核销即激活 / 续期，钱包不动、面值忽略
    let accepted = if claimed.subscription_plan_id.is_some() {
        let plan: okapi_store::subscriptions::SubPlan = serde_json::from_value(
            claimed
                .subscription_snapshot
                .take()
                .ok_or_else(AppError::internal)?,
        )
        .map_err(|_| AppError::internal())?;
        let enqueued = okapi_ledger::subscriptions::enqueue(
            &mut tx,
            user_id,
            &plan,
            &format!("redeem:{}", claimed.code_id),
            "system:redeem",
            false,
        )
        .await;
        match enqueued {
            Ok(id) => Accepted::Subscription(id),
            Err(error) => {
                tx.rollback().await.map_err(okapi_store::StoreError::from)?;
                return Err(error.into());
            }
        }
    } else {
        Accepted::Wallet(accept_wallet_redemption(&mut tx, user_id, &claimed).await?)
    };
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    Ok((guard, claimed, accepted))
}

/// Keep optional plan benefits in the same transaction as code consumption.
pub(super) async fn accept_wallet_redemption(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: i64,
    claimed: &okapi_store::admin::ClaimedRedemption,
) -> Result<uuid::Uuid, AppError> {
    let amount = okapi_domain::Money::from_micros(claimed.amount_micro);
    let operation_id = okapi_ledger::transfers::credit_in_tx(
        tx,
        user_id,
        amount,
        "adjust",
        "system:redeem",
        json!({"tags": ["redemption"], "code_id": claimed.code_id, "plan_code": claimed.plan_code}),
    )
    .await?;
    if let Some(group) = &claimed.grant_group {
        sqlx::query!("INSERT INTO user_groups(user_id,group_code,priority) VALUES ($1,$2,0) ON CONFLICT (user_id,group_code) DO NOTHING",user_id,group)
            .execute(&mut **tx).await.map_err(okapi_store::StoreError::from)?;
    }
    if let Some(days) = claimed.balance_valid_days {
        let expires = chrono::Utc::now() + chrono::Duration::days(i64::from(days));
        // 到期是整个钱包的一个时间点，到点连旧钱一起清零：兑换码只能把它往后推，不能缩短用户
        // 已有的有效期（已有 300 天的用户兑一个 30 天的码，不该在第 30 天丢掉旧钱）。
        // GREATEST 忽略 NULL：原本没有有效期的钱包照旧按这个码设置。
        sqlx::query!(
            "UPDATE users SET balance_expires_at=GREATEST(balance_expires_at,$2),updated_at=now() WHERE id=$1",
            user_id,
            expires
        )
        .execute(&mut **tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    }
    Ok(operation_id)
}
