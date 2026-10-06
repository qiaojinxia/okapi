use super::{TickReport, end_locked, recover, roll_locked};
use crate::{BalanceLedger, LedgerError, holds::UserGuard};
use chrono::{DateTime, Utc};
use okapi_store::subscriptions as store;
use sqlx::PgPool;

enum Changed {
    None,
    Rolled,
    Expired { group_revoked: bool },
}

async fn maintain(
    pg: &PgPool,
    ledger: &BalanceLedger,
    id: i64,
    uid: i64,
    now: DateTime<Utc>,
) -> Result<Changed, LedgerError> {
    let mut guard = UserGuard::acquire(pg, uid).await?;
    guard.synchronize(ledger).await?;
    let Some(current) = store::by_id(guard.connection()?, id).await? else {
        return Ok(Changed::None);
    };
    if current.status != 1 {
        return Ok(Changed::None);
    }
    // The scan is advisory: a renewal may have committed while we waited for
    // the user lock. Decide expiry/window rollover only from this fresh state.
    if current.expires_at <= now {
        return Ok(
            match end_locked(&mut guard, ledger, id, 2, "system:worker").await? {
                Some(ended) => Changed::Expired {
                    group_revoked: ended.granted_group,
                },
                None => Changed::None,
            },
        );
    }
    if current.window_end > now {
        return Ok(Changed::None);
    }
    roll_locked(&mut guard, ledger, current, now, "system:worker").await?;
    Ok(Changed::Rolled)
}

pub async fn tick(
    pg: &PgPool,
    ledger: &BalanceLedger,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<TickReport, LedgerError> {
    let mut report = TickReport {
        group_changed: recover(pg, ledger, limit).await?,
        ..TickReport::default()
    };
    for sub in store::due(pg, now, limit.clamp(1, 1000)).await? {
        match maintain(pg, ledger, sub.id, sub.user_id, now).await {
            Ok(Changed::None) => {}
            Ok(Changed::Rolled) => report.rolled += 1,
            Ok(Changed::Expired { group_revoked }) => {
                report.expired += 1;
                report.group_changed |= group_revoked;
            }
            Err(error) => {
                report.failed += 1;
                // Do not postpone an instance that another worker has already
                // renewed or advanced since our scan. Failed retry bookkeeping
                // is reported, but must not abort other users in this batch.
                if let Err(save_error) = sqlx::query!(
                    "UPDATE user_subscriptions SET maintenance_retry_after=$2::timestamptz+interval '60 seconds' WHERE id=$1 AND status=1 AND window_start=$3 AND window_end=$4 AND expires_at=$5",
                    sub.id,now,sub.window_start,sub.window_end,sub.expires_at
                ).execute(pg).await {
                    tracing::error!(subscription_id=sub.id,%save_error,"subscription retry scheduling failed");
                }
                tracing::error!(subscription_id=sub.id,%error,"subscription maintenance deferred");
            }
        }
    }
    Ok(report)
}
