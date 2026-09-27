//! PG accepts each source once; subscription state, events and pending balance
//! recovery commit together. Redis failures remain recoverable by any worker.
pub(crate) mod effects;
mod grants;
mod maintenance;
pub use grants::{enqueue, finish, recover, request};
pub use maintenance::tick;

use crate::error::LedgerError;
use crate::holds::UserGuard;
use crate::redis::BalanceLedger;
use okapi_domain::Money;
use okapi_store::subscriptions::{self as store, SubPlan, Subscription};
use sqlx::{Connection, PgPool};

/// 授予结果。
#[derive(Debug)]
pub enum Granted {
    /// 新订阅：池已注资、`sub_grant` 已记。
    Activated(Subscription),
    /// 同套餐续期：只延了可用截止，窗口与池余额不动。
    Renewed(Subscription),
}

impl Granted {
    #[must_use]
    pub fn subscription(&self) -> &Subscription {
        match self {
            Self::Activated(s) | Self::Renewed(s) => s,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Activated(_) => "activated",
            Self::Renewed(_) => "renewed",
        }
    }

    /// 用户分组集合是否变了（调用方据此失效鉴权缓存）。
    #[must_use]
    pub const fn group_changed(&self) -> bool {
        matches!(self, Self::Activated(s) if s.granted_group)
    }
}

pub struct Receipt {
    pub id: uuid::Uuid,
    pub pending: bool,
    pub granted: Option<Granted>,
}

/// Direct callers get an immutable source receipt; retrying cannot renew again.
pub async fn grant(
    pg: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
    plan: &SubPlan,
    source: &str,
    actor: &str,
) -> Result<Granted, LedgerError> {
    request(pg, ledger, user_id, plan, source, actor)
        .await?
        .granted
        .ok_or(LedgerError::HoldRecoveryRequired)
}

pub async fn end(
    pg: &PgPool,
    ledger: &BalanceLedger,
    id: i64,
    status: i16,
    actor: &str,
) -> Result<Option<Subscription>, LedgerError> {
    if !matches!(status, 2 | 3) {
        return Err(LedgerError::InvalidHold("subscription_status"));
    }
    let Some(before) = store::by_id(pg, id).await? else {
        return Ok(None);
    };
    let mut guard = UserGuard::acquire(pg, before.user_id).await?;
    guard.synchronize(ledger).await?;
    end_locked(&mut guard, ledger, id, status, actor).await
}

async fn end_locked(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    id: i64,
    status: i16,
    actor: &str,
) -> Result<Option<Subscription>, LedgerError> {
    let frozen = guard.subscription_frozen(ledger).await?;
    let mut tx = guard.connection().begin().await?;
    let Some(mut sub) = store::finish_in_tx(&mut tx, id, status).await? else {
        return Ok(None);
    };
    effects::reset(
        &mut tx,
        sub.user_id,
        Money::ZERO,
        frozen,
        "sub_expire",
        actor,
        serde_json::json!({"plan_code":sub.plan_code,"subscription_id":id,"status":status,
        "group_revoked":sub.granted_group.then(||sub.group_code.clone()).flatten()}),
    )
    .await?;
    // Ending a conflicting subscription immediately makes accepted grants eligible.
    sqlx::query!(
        "UPDATE subscription_grants SET retry_after=NULL WHERE user_id=$1 AND applied_at IS NULL",
        sub.user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if let Err(error) = guard.synchronize(ledger).await {
        tracing::error!(subscription_id=id,%error,"subscription end awaiting balance recovery");
    }
    sub.status = status;
    Ok(Some(sub))
}

/// Recheck the window under the user lock. A stale tick cannot refill a window
/// another worker has already advanced and the user has subsequently consumed.
pub async fn roll(
    pg: &PgPool,
    ledger: &BalanceLedger,
    sub: &Subscription,
    now: chrono::DateTime<chrono::Utc>,
    actor: &str,
) -> Result<Subscription, LedgerError> {
    let mut guard = UserGuard::acquire(pg, sub.user_id).await?;
    guard.synchronize(ledger).await?;
    let current = store::by_id(guard.connection(), sub.id)
        .await?
        .ok_or(LedgerError::UserNotFound)?;
    if current.status != 1
        || current.window_end > now
        || current.window_start != sub.window_start
        || current.window_end != sub.window_end
    {
        return Ok(current);
    }
    roll_locked(&mut guard, ledger, current, now, actor).await
}

async fn roll_locked(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    current: Subscription,
    now: chrono::DateTime<chrono::Utc>,
    actor: &str,
) -> Result<Subscription, LedgerError> {
    if current.expires_at <= now {
        return Err(LedgerError::InvalidHold("subscription_expired"));
    }
    let frozen = guard.subscription_frozen(ledger).await?;
    let (ws, we) = store::advance_window(current.window_end, current.period(), now);
    let mut tx = guard.connection().begin().await?;
    sqlx::query!("UPDATE user_subscriptions SET window_start=$2,window_end=$3,updated_at=now(),maintenance_retry_after=NULL WHERE id=$1 AND status=1",current.id,ws,we).execute(&mut *tx).await?;
    effects::reset(&mut tx,current.user_id,Money::from_micros(current.quota_micro),frozen,"sub_reset",actor,
        serde_json::json!({"plan_code":current.plan_code,"subscription_id":current.id,"window_start":ws,"window_end":we})).await?;
    tx.commit().await?;
    if let Err(error) = guard.synchronize(ledger).await {
        tracing::error!(subscription_id=current.id,%error,"subscription window awaiting balance recovery");
    }
    Ok(Subscription {
        window_start: ws,
        window_end: we,
        ..current
    })
}

/// 一轮滚窗 / 到期的结果。
#[derive(Debug, Default, Clone, Copy)]
pub struct TickReport {
    pub rolled: u32,
    pub expired: u32,
    /// Failed users are deferred without preventing unrelated maintenance.
    pub failed: u32,
    /// 有订阅收回了分组：调用方需失效鉴权缓存。
    pub group_changed: bool,
}
