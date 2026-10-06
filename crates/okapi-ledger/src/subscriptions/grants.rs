use super::{Granted, Receipt, effects};
use crate::{BalanceLedger, LedgerError, holds::UserGuard};
use okapi_domain::Money;
use okapi_store::subscriptions::{self as store, ActivateOutcome, SubPlan};
use sqlx::{Connection, PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Business acceptance is PG-only. A paid callback can defer a plan conflict;
/// a manual grant or code claim rejects it before consuming its source.
pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    uid: i64,
    plan: &SubPlan,
    source: &str,
    actor: &str,
    defer_conflict: bool,
) -> Result<Uuid, LedgerError> {
    if source.is_empty()
        || source.len() > 96
        || actor.len() > 64
        || plan.duration_days <= 0
        || !(1..=3).contains(&plan.period)
        || !(1..=crate::holds::MAXIMUM_MICROS).contains(&plan.quota_micro)
    {
        return Err(LedgerError::InvalidHold("subscription_grant"));
    }
    let old = sqlx::query!(
        "SELECT id,plan_snapshot FROM subscription_grants WHERE user_id=$1 AND source=$2",
        uid,
        source
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(old) = old {
        let accepted: SubPlan = serde_json::from_value(old.plan_snapshot)
            .map_err(|_| LedgerError::InvalidHold("subscription_snapshot"))?;
        if accepted.id != plan.id {
            return Err(LedgerError::ReservationConflict);
        }
        return Ok(old.id);
    }
    let now = chrono::Utc::now();
    store::expiry_after(now, plan.duration_days)?;
    if !defer_conflict {
        let active=sqlx::query!("SELECT plan_id,plan_code_snapshot,quota_micro,period_snapshot,group_code_snapshot,expires_at FROM user_subscriptions WHERE user_id=$1 AND status=1",uid).fetch_optional(&mut **tx).await?;
        if let Some(active) = &active {
            store::expiry_after(active.expires_at.max(now), plan.duration_days)?;
        }
        if let Some(active) = active
            && (active.plan_id != plan.id
                || active.quota_micro != plan.quota_micro
                || active.period_snapshot != plan.period
                || active.group_code_snapshot != plan.group_code)
        {
            return Err(LedgerError::SubscriptionActive(active.plan_code_snapshot));
        }
    }
    let id = Uuid::new_v4();
    let snapshot = serde_json::to_value(plan)
        .map_err(|_| LedgerError::InvalidHold("subscription_snapshot"))?;
    sqlx::query!("INSERT INTO subscription_grants(id,user_id,source,plan_snapshot,actor) VALUES($1,$2,$3,$4,$5)",id,uid,source,snapshot,actor)
        .execute(&mut **tx).await?;
    Ok(id)
}

pub async fn request(
    pg: &PgPool,
    ledger: &BalanceLedger,
    uid: i64,
    plan: &SubPlan,
    source: &str,
    actor: &str,
) -> Result<Receipt, LedgerError> {
    let mut guard = UserGuard::acquire(pg, uid).await?;
    let mut tx = guard.connection()?.begin().await?;
    let id = enqueue(&mut tx, uid, plan, source, actor, false).await?;
    tx.commit().await?;
    Ok(finish(&mut guard, ledger, uid, id).await)
}

pub async fn finish(guard: &mut UserGuard, ledger: &BalanceLedger, uid: i64, id: Uuid) -> Receipt {
    let result = process(guard, ledger, uid, id).await;
    match result {
        Ok(granted) => {
            let pending = guard.synchronize(ledger).await.is_err();
            Receipt {
                id,
                pending,
                granted: Some(granted),
            }
        }
        Err(error) => {
            let code = if matches!(error, LedgerError::SubscriptionActive(_)) {
                "subscription_active"
            } else {
                "delivery_pending"
            };
            let saved = async {
                sqlx::query!(
                    "UPDATE subscription_grants SET last_error=$2 WHERE id=$1 AND applied_at IS NULL",
                    id, code
                ).execute(guard.connection()?).await?;
                Ok::<_, LedgerError>(())
            }.await;
            if let Err(save_error) = saved {
                tracing::warn!(%id,%save_error,"subscription delivery status pending");
            }
            tracing::error!(user_id=uid,operation_id=%id,%error,"subscription grant awaiting recovery");
            Receipt {
                id,
                pending: true,
                granted: None,
            }
        }
    }
}

async fn process(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    uid: i64,
    id: Uuid,
) -> Result<Granted, LedgerError> {
    guard.synchronize(ledger).await?;
    // Preserve acceptance order for this user, including older paid deliveries.
    let rows=sqlx::query!("SELECT id FROM subscription_grants WHERE user_id=$1 AND applied_at IS NULL AND sequence <= (SELECT sequence FROM subscription_grants WHERE id=$2 AND user_id=$1) ORDER BY sequence LIMIT 32",uid,id)
        .fetch_all(guard.connection()?).await?;
    for row in rows {
        apply(guard, ledger, uid, row.id).await?;
    }
    let row = sqlx::query!(
        "SELECT subscription_id,outcome FROM subscription_grants WHERE id=$1 AND user_id=$2",
        id,
        uid
    )
    .fetch_one(guard.connection()?)
    .await?;
    let sub = store::by_id(
        guard.connection()?,
        row.subscription_id
            .ok_or(LedgerError::HoldRecoveryRequired)?,
    )
    .await?
    .ok_or(LedgerError::UserNotFound)?;
    match row.outcome.as_deref() {
        Some("activated") => Ok(Granted::Activated(sub)),
        Some("renewed") => Ok(Granted::Renewed(sub)),
        _ => Err(LedgerError::HoldRecoveryRequired),
    }
}

async fn apply(
    guard: &mut UserGuard,
    ledger: &BalanceLedger,
    uid: i64,
    id: Uuid,
) -> Result<(), LedgerError> {
    guard.synchronize(ledger).await?;
    let frozen = guard.subscription_frozen(ledger).await?;
    let mut tx = guard.connection()?.begin().await?;
    let row=sqlx::query!("SELECT source,plan_snapshot,actor FROM subscription_grants WHERE id=$1 AND user_id=$2 AND applied_at IS NULL FOR UPDATE",id,uid)
        .fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Ok(());
    };
    let plan: SubPlan = serde_json::from_value(row.plan_snapshot)
        .map_err(|_| LedgerError::InvalidHold("subscription_snapshot"))?;
    let outcome =
        store::activate_in_tx(&mut tx, uid, &plan, chrono::Utc::now(), &row.source).await?;
    let (sub, kind) = match outcome {
        ActivateOutcome::Activated(sub) => {
            effects::reset(&mut tx,uid,Money::from_micros(sub.quota_micro),frozen,"sub_grant",&row.actor,
                serde_json::json!({"plan_code":plan.plan_code,"source":row.source,"subscription_id":sub.id,"operation_id":id})).await?;
            (sub, "activated")
        }
        ActivateOutcome::Renewed(sub) => {
            effects::schedule(&mut tx, uid).await?;
            (sub, "renewed")
        }
        ActivateOutcome::Conflict { active_plan_code } => {
            return Err(LedgerError::SubscriptionActive(active_plan_code));
        }
    };
    sqlx::query!("UPDATE subscription_grants SET subscription_id=$2,outcome=$3,applied_at=now(),last_error=NULL WHERE id=$1",id,sub.id,kind)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    // A failure here leaves a durable effect; do not roll back accepted PG state.
    if let Err(error) = guard.synchronize(ledger).await {
        tracing::error!(user_id=uid,%id,%error,"subscription balance awaiting repair");
    }
    Ok(())
}

pub async fn recover(pg: &PgPool, ledger: &BalanceLedger, limit: i64) -> Result<bool, LedgerError> {
    let rows=sqlx::query!("SELECT user_id,MAX(sequence) AS sequence FROM subscription_grants WHERE applied_at IS NULL GROUP BY user_id HAVING BOOL_AND(retry_after IS NULL OR retry_after<=now()) ORDER BY MIN(sequence) LIMIT $1",limit.clamp(1,1000)).fetch_all(pg).await?;
    let changed = !rows.is_empty();
    for row in rows {
        let result = async {
            let mut guard = UserGuard::acquire(pg, row.user_id).await?;
            let id = sqlx::query_scalar!(
                "SELECT id FROM subscription_grants WHERE sequence=$1",
                row.sequence
            )
            .fetch_one(guard.connection()?)
            .await?;
            let receipt = finish(&mut guard, ledger, row.user_id, id).await;
            if receipt.pending {
                defer(guard.connection()?, row.user_id).await?;
            }
            Ok::<_, LedgerError>(())
        }
        .await;
        if let Err(error) = result {
            if let Err(save_error) = defer(pg, row.user_id).await {
                tracing::warn!(user_id=row.user_id,%save_error,"subscription retry scheduling failed");
            }
            tracing::warn!(user_id=row.user_id,%error,"subscription user recovery pending");
        }
    }
    let pending = sqlx::query_scalar!(
        "SELECT user_id FROM subscription_sync WHERE retry_after IS NULL OR retry_after<=now() ORDER BY created_at LIMIT $1",
        limit.clamp(1, 1000)
    )
    .fetch_all(pg)
    .await?;
    for uid in pending {
        let result = async {
            let mut guard = UserGuard::acquire(pg, uid).await?;
            guard.synchronize(ledger).await
        }
        .await;
        if let Err(error) = result {
            if let Err(save_error) = defer(pg, uid).await {
                tracing::warn!(user_id=uid,%save_error,"subscription retry scheduling failed");
            }
            tracing::warn!(user_id=uid,%error,"subscription repair pending");
        }
    }
    Ok(changed)
}

async fn defer<'e, E>(executor: E, uid: i64) -> Result<(), LedgerError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    sqlx::query!("WITH grants AS (UPDATE subscription_grants SET retry_after=now()+interval '60 seconds' WHERE user_id=$1 AND applied_at IS NULL RETURNING id) UPDATE subscription_sync SET retry_after=now()+interval '60 seconds' WHERE user_id=$1",uid)
        .execute(executor).await?;
    Ok(())
}
